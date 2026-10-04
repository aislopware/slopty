//! One Codex thread, shared with its TUI, mapped onto the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Sans-IO. The worker holds the connection to the app-server and routes what it hears by
//! thread; this turns a thread's [`ServerNotification`]s and [`ServerRequest`]s into
//! [`Action`]s, and what a client asks of the thread into the requests and answers that go
//! back. It keeps only what the mapping needs: which of Slopty's turns each of Codex's is, the
//! approvals open, and who answered which.
//!
//! - **The thread.** A Codex thread is one thread, its id derived from Codex's own ([`thread_of`]),
//!   so the same Codex thread is the same thread across a worker restart. What Codex already holds
//!   of it (the turns a `thread/resume` brings back) is read first.
//! - **Turns and items.** Each of Codex's turns is the next of Slopty's, counted from 1. An item
//!   keeps Codex's id. `item/completed` is authoritative and replaces what came before.
//! - **Approvals.** Codex asks every client that follows the thread, under one request id, and the
//!   first answer settles it (`docs/decisions/agents.md`). A request is open here until
//!   `serverRequest/resolved` says so, answered by this worker's client when it answered first,
//!   else by the Codex TUI beside it. Its answers are Codex's `availableDecisions`, in Codex's
//!   order.
//! - **Questions.** A `request_user_input` is a request carrying its questions, each with the
//!   answers Codex offers, answered by one choice for them all (`detail::Answer`); each answer goes
//!   back under its question's id. A question for a secret is left to Codex's own terminal, since
//!   an answer given here is kept in the thread's log.
//! - **Forms.** An MCP server's form ([`super::form`]) is a request whose questions are its fields,
//!   with declining and cancelling as its answers; its answers go back as the form's content. A
//!   page to open or a device check is left to Codex's own terminal.
//! - **The queue.** A message queued while a turn runs waits here, where it can be changed, taken
//!   back, moved in the queue or sent at once as a steer. Otherwise it goes as the next turn once
//!   Codex says the turn under way ended ([`Shared::next_queued`]): Codex's own TUI queues in
//!   itself, and the app-server has no queue of its own.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde_json::Value;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::detail::{
    AgentDetail, Answer, Clip, EditDetail, ExecDetail, ExecStatus, Hunk, McpDetail, Offered,
    Question, header_heading,
};
use slopty_proto::thread::wire::PastSession;
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, Cap, Changed, Choice, Clipped, Compaction, Delivery, Drive,
    Effect, Fork, IntentId, Item, ItemBody, ItemId, Limit, Link, Liveness, Meters, Notice, PartKey,
    Patch, Pending, PendingState, Phase, Plan, Request, RequestState, Retry, Status, Step,
    ThreadId, ThreadMeta, ToolCall, ToolDetail, ToolState, Turn, TurnId, TurnState, Usage,
    UserMessage, Wait, kind,
};

use super::form::Form;
use super::protocol::{
    self as p, CommandExecutionApprovalDecision, CommandExecutionStatus,
    FileChangeApprovalDecision, PatchApplyStatus, RequestId, ServerNotification, ServerRequest,
    ThreadActiveFlag, ThreadItem, ThreadStatus, TurnStatus, UserInput,
};
use crate::attach::Attached;

/// What a Codex thread can do through Slopty.
pub const CAPS: [&str; 11] = [
    Cap::APPROVALS,
    Cap::CONTINUE,
    Cap::FORK,
    Cap::INTERRUPT,
    Cap::LIVE_TEXT,
    Cap::LIVE_TUI,
    Cap::QUEUE,
    Cap::REWIND,
    Cap::SCHEDULE,
    Cap::SLEEP,
    Cap::STEER,
];

/// A form's answer that declines it, and the one that cancels it. Their ids start with a
/// colon, so words typed into a form's one field are not taken for them.
pub const DECLINE: &str = ":decline";
/// See [`DECLINE`].
pub const CANCEL: &str = ":cancel";

/// Who answered a request this worker's client did not: the Codex TUI beside it, or another
/// client of the app-server.
pub const ELSEWHERE: &str = "Codex";

/// Prose: answers, reasoning, plans.
const PROSE: Clip = Clip { lines: 400, chars: 32_000 };
/// A command's output, its tail kept.
const OUTPUT: Clip = Clip { lines: 40, chars: 4_000 };

/// The thread of Codex thread `native`.
#[must_use]
pub fn thread_of(native: &str) -> ThreadId {
    ThreadId::derived(&["codex thread", native])
}

/// What a start of a Codex thread takes to take thread `native` up again rather than begin one:
/// `resume <native>`, the words of Codex's own `codex resume`.
pub const RESUME: &str = "resume";

/// The arguments of a start that takes Codex thread `native` up again.
#[must_use]
pub fn resume_args(native: &str) -> Vec<String> {
    vec![RESUME.to_owned(), native.to_owned()]
}

/// The Codex thread a start's `args` take up again, when they are [`resume_args`].
#[must_use]
pub fn resumed(args: &[String]) -> Option<&str> {
    match args {
        [word, native] if word == RESUME && !native.trim().is_empty() => Some(native),
        _ => None,
    }
}

/// What a Slopty terminal tells the programs in it of itself: which terminal it is, its proof,
/// and the project task it runs.
pub const TERMINAL_ENV: [&str; 4] = [
    slopty_proto::ctl::SESSION_ENV,
    slopty_proto::ctl::SESSION_TOKEN_ENV,
    slopty_proto::project::PROJECT_ENV,
    slopty_proto::project::TASK_ENV,
];

/// The configuration every thread Slopty starts, takes up again or forks is loaded with: each of
/// [`TERMINAL_ENV`] set empty in the commands it runs (`shell_environment_policy.set`).
///
/// Codex's daemon runs every thread's commands with its own environment, which is that of
/// whatever started it, a Codex TUI in a Slopty terminal among them. Without this, every thread's
/// tools would claim that terminal and its project task. Set empty, the variables name no
/// terminal, so Slopty's CLI in such a command speaks as an agent, never for the person. Each is
/// its own key, so it adds to the person's own policy and replaces none of it.
#[must_use]
pub fn unclaimed() -> BTreeMap<String, Value> {
    TERMINAL_ENV
        .iter()
        .map(|var| (format!("shell_environment_policy.set.{var}"), Value::String(String::new())))
        .collect()
}

/// The name Slopty's tools go by among a thread's MCP servers.
pub const TOOLS: &str = "slopty";

/// The configuration a server task's thread is loaded with, in place of [`unclaimed`].
///
/// Each of [`TERMINAL_ENV`] is set in the commands it runs to the seat's value, so Slopty's CLI
/// there speaks as the seat as it would in the seat's terminal; and, with `relay`, Slopty's tools
/// are among its MCP servers (`<relay> mcp`), given the seat's variables (`env`) as their own.
///
/// Codex hands an MCP server only the variables its configuration names, and the daemon's
/// environment is nobody's seat, so the values go as the server's own `env`, not `env_vars`.
#[must_use]
pub fn seated(env: &[(String, String)], relay: Option<&str>) -> BTreeMap<String, Value> {
    let value = |name: &str| env.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone());
    let mut config: BTreeMap<String, Value> = TERMINAL_ENV
        .iter()
        .map(|var| {
            let set = Value::String(value(var).unwrap_or_default());
            (format!("shell_environment_policy.set.{var}"), set)
        })
        .collect();
    if let Some(relay) = relay {
        let vars: serde_json::Map<String, Value> =
            env.iter().map(|(n, v)| (n.clone(), Value::String(v.clone()))).collect();
        config.insert(format!("mcp_servers.{TOOLS}.command"), Value::String(relay.to_owned()));
        config.insert(format!("mcp_servers.{TOOLS}.args"), serde_json::json!(["mcp"]));
        config.insert(format!("mcp_servers.{TOOLS}.env"), Value::Object(vars));
    }
    config
}

/// What asks the app-server for a new thread in `cwd` (`thread/start`).
///
/// It names `model` when one is given and [`unclaimed`], and nothing else: the approval policy,
/// the sandbox and the rest are the person's own Codex configuration's, so a thread Slopty starts
/// is loosened in nothing.
#[must_use]
pub fn start(cwd: &str, model: Option<&str>) -> p::ThreadStartParams {
    p::ThreadStartParams {
        cwd: Some(cwd.to_owned()),
        model: model.map(str::trim).filter(|m| !m.is_empty()).map(str::to_owned),
        config: Some(unclaimed()),
        ..p::ThreadStartParams::default()
    }
}

/// An approval or question open on the thread.
#[derive(Clone, Debug)]
struct Open {
    /// Codex's id for it, which the answer carries.
    id: RequestId,
    /// The call it is about.
    item: Option<ItemId>,
    /// What it asks, in a line.
    title: String,
    /// Its [`Request`] kind.
    kind: &'static str,
    /// What each choice answers, by its id: the decision as Codex takes it.
    answers: BTreeMap<String, Value>,
    /// The questions it asks, each with Codex's id for it, which its answer is keyed by.
    questions: Vec<(String, Question)>,
    /// For an MCP server's form: it, whose content the answers fill.
    form: Option<Form>,
    /// Who answered from here, and with what, before Codex said it was settled.
    answered: Option<(Answerer, String)>,
}

/// What goes to the app-server for a message the person sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Send {
    /// A new turn.
    Start(Box<p::TurnStartParams>),
    /// Into the turn under way.
    Steer(Box<p::TurnSteerParams>),
    /// Held until the turn under way ends: these actions show it waiting.
    Held(Vec<Action>),
}

/// A Codex thread as Slopty shows it.
#[derive(Debug)]
pub struct Shared {
    meta: ThreadMeta,
    /// Slopty's turn for each of Codex's, by Codex's id.
    turns: HashMap<String, TurnId>,
    /// The turn under way, by Codex's id.
    current: Option<String>,
    /// Requests open, by their [`AskId`].
    open: BTreeMap<AskId, Open>,
    meters: Meters,
    /// What the turn under way has taken so far.
    usage: Usage,
    /// How the last turn ended, for the phase at rest.
    last_end: Option<TurnState>,
    /// Errors noted so far, which number their items.
    errors: u32,
    /// The retries Codex said it makes in the turn under way.
    retries: u32,
    /// The thread's status as Codex last said it, and since when.
    said: Option<(ThreadStatus, WallMs)>,
    /// Each turn as it began, told again once its input is known.
    begun: HashMap<TurnId, Turn>,
    /// The calls begun and not yet whole, as last told, so a request about one marks it
    /// waiting on the person: Codex begins a call before it asks about it.
    calls: HashMap<ItemId, Item>,
    /// Messages held until the turn under way ends, in their order.
    queued: VecDeque<(Pending, Vec<Attached>)>,
}

impl Shared {
    /// Codex thread `thread`, its TUI (when Slopty runs one) in terminal `terminal`, with the
    /// actions that begin it from what Codex holds of it.
    #[must_use]
    pub fn new(thread: &p::Thread, terminal: Option<SessionId>) -> (Self, Vec<Action>) {
        let id = thread_of(&thread.id);
        let mut caps: Vec<Cap> = CAPS.iter().map(|c| Cap::named(c)).collect();
        caps.sort();
        let mut facts = BTreeMap::new();
        facts.insert("model-provider".to_owned(), thread.model_provider.clone());
        if let Some(git) = &thread.git_info
            && let Some(branch) = &git.branch
        {
            facts.insert("branch".to_owned(), branch.clone());
        }
        for (fact, value) in
            [("agent-nickname", &thread.agent_nickname), ("agent-role", &thread.agent_role)]
        {
            if let Some(value) = value.clone().filter(|v| !v.is_empty()) {
                facts.insert(fact.to_owned(), value);
            }
        }
        // Codex records the thread a fork came from, but not the turn it branched after.
        let forked_from = thread
            .forked_from_id
            .as_deref()
            .filter(|from| !from.is_empty())
            .map(|from| Fork { thread: thread_of(from), turn: None });
        let origin = if thread.parent_thread_id.is_some() {
            ThreadMeta::SUBAGENT
        } else if forked_from.is_some() {
            ThreadMeta::FORK
        } else {
            ThreadMeta::PERSON
        };
        let meta = ThreadMeta {
            modes: Vec::new(),
            id,
            agent: AgentId::named(AgentId::CODEX),
            agent_version: thread.cli_version.clone(),
            native: thread.id.clone(),
            cwd: thread.cwd.clone(),
            title: title_of(thread),
            terminal,
            parent: None,
            origin: origin.to_owned(),
            forked_from,
            drive: Drive::named(Drive::SHARED),
            caps,
            models: Vec::new(),
            facts,
            created_ms: seconds(thread.created_at),
        };
        let mut shared = Self {
            meta,
            turns: HashMap::new(),
            current: None,
            open: BTreeMap::new(),
            meters: Meters {
                model: thread.model.clone(),
                model_id: thread.model.clone(),
                effort: thread.reasoning_effort.as_ref().map(wire).filter(|e| !e.is_empty()),
                ..Meters::default()
            },
            usage: Usage::default(),
            last_end: None,
            errors: 0,
            retries: 0,
            said: None,
            begun: HashMap::new(),
            calls: HashMap::new(),
            queued: VecDeque::new(),
        };
        let mut actions = vec![Action::Meta(Box::new(shared.meta.clone()))];
        for turn in &thread.turns {
            actions.extend(shared.turn_started(turn));
            for item in &turn.items {
                actions.extend(shared.item(&turn.id, item, WallMs::ZERO, true));
            }
            if !matches!(turn.status, TurnStatus::InProgress) {
                actions.extend(shared.turn_completed(turn));
            }
        }
        actions.push(Action::MetersSet(shared.meters.clone()));
        actions.push(shared.status(thread.status.clone(), seconds(thread.updated_at)));
        (shared, actions)
    }

    /// What the thread is.
    #[must_use]
    pub const fn meta(&self) -> &ThreadMeta {
        &self.meta
    }

    /// Codex's turn under way, to steer or interrupt.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// A notification about this thread, heard at `now`.
    pub fn notification(&mut self, note: &ServerNotification, now: WallMs) -> Vec<Action> {
        match note {
            ServerNotification::ThreadStatusChanged(changed) => {
                vec![self.status(changed.status.clone(), now)]
            }
            ServerNotification::ThreadNameUpdated(named) => {
                let Some(name) = named.thread_name.clone().filter(|n| !n.is_empty()) else {
                    return Vec::new();
                };
                self.meta.title = name;
                vec![Action::Meta(Box::new(self.meta.clone()))]
            }
            ServerNotification::ThreadClosed(_) => {
                let status = Status {
                    phase: Phase::Idle,
                    wait: None,
                    liveness: Liveness::Exited { resumable: true },
                    since_ms: now,
                };
                vec![Action::Status(status)]
            }
            ServerNotification::TurnStarted(started) => {
                self.usage = Usage::default();
                self.retries = 0;
                self.turn_started(&started.turn)
            }
            ServerNotification::TurnCompleted(done) => self.turn_completed(&done.turn),
            ServerNotification::ItemStarted(started) => {
                self.item(&started.turn_id, &started.item, millis(started.started_at_ms), false)
            }
            ServerNotification::ItemCompleted(done) => {
                self.item(&done.turn_id, &done.item, millis(done.completed_at_ms), true)
            }
            ServerNotification::ItemAgentMessageDelta(d) => {
                append(&d.item_id, PartKey::Body, &d.delta)
            }
            ServerNotification::ItemReasoningSummaryTextDelta(d) => {
                append(&d.item_id, PartKey::Body, &d.delta)
            }
            ServerNotification::ItemReasoningTextDelta(d) => {
                append(&d.item_id, PartKey::Body, &d.delta)
            }
            ServerNotification::ItemPlanDelta(d) => append(&d.item_id, PartKey::Body, &d.delta),
            ServerNotification::ItemCommandExecutionOutputDelta(d) => {
                append(&d.item_id, PartKey::Output, &d.delta)
            }
            ServerNotification::ItemFileChangeOutputDelta(d) => {
                append(&d.item_id, PartKey::Output, &d.delta)
            }
            ServerNotification::TurnPlanUpdated(plan) => {
                let steps = plan
                    .plan
                    .iter()
                    .map(|step| Step {
                        id: None,
                        text: step.step.clone(),
                        status: wire(&step.status),
                    })
                    .collect();
                let text = plan.explanation.as_deref().map(Clipped::whole);
                vec![Action::PlanSet(Some(Plan { text, steps }))]
            }
            ServerNotification::ThreadTokenUsageUpdated(usage) => {
                self.usage.add(&usage_of(&usage.token_usage.last));
                let last = &usage.token_usage.last;
                self.meters.context_tokens = u64::try_from(last.total_tokens).ok();
                self.meters.context_window =
                    usage.token_usage.model_context_window.and_then(|w| u64::try_from(w).ok());
                vec![Action::MetersSet(self.meters.clone())]
            }
            ServerNotification::ServerRequestResolved(resolved) => {
                self.resolved(&resolved.request_id)
            }
            ServerNotification::TurnDiffUpdated(diff) => self.turn_diff(&diff.turn_id, &diff.diff),
            ServerNotification::ModelRerouted(rerouted) => {
                let turn = self.turn(&rerouted.turn_id);
                let mut actions = Vec::new();
                if let Some(begun) = self.begun.get_mut(&turn)
                    && !begun.models.contains(&rerouted.to_model)
                {
                    begun.models.push(rerouted.to_model.clone());
                    actions.push(Action::TurnStarted(begun.clone()));
                }
                let text = format!(
                    "Codex moved this turn from {} to {} ({})",
                    rerouted.from_model,
                    rerouted.to_model,
                    wire(&rerouted.reason)
                );
                actions.extend(self.notice(turn, Notice::INFO, &text, now));
                actions
            }
            ServerNotification::Warning(warning) => {
                let turn = self.current.clone().map_or(TurnId::BEFORE, |t| self.turn(&t));
                self.notice(turn, Notice::INFO, &warning.message, now)
            }
            ServerNotification::AccountRateLimitsUpdated(updated) => {
                self.rate_limits(&updated.rate_limits)
            }
            ServerNotification::Error(error) => {
                self.errors = self.errors.saturating_add(1);
                // Codex numbers no attempt and says no wait: the count is the turn's own.
                let retry = error.will_retry.then(|| {
                    self.retries = self.retries.saturating_add(1);
                    Retry { attempt: self.retries.saturating_add(1), max: None, in_ms: None }
                });
                let item = Item {
                    id: ItemId(format!("error:{}", self.errors)),
                    turn: self.turn(&error.turn_id),
                    at_ms: now,
                    body: ItemBody::Notice(Notice {
                        kind: if retry.is_none() && limited(error.error.codex_error_info) {
                            Notice::LIMIT
                        } else {
                            Notice::API_ERROR
                        }
                        .to_owned(),
                        text: Clipped::whole(&error.error.message),
                        retry,
                    }),
                };
                vec![Action::ItemCompleted(item)]
            }
            _ => Vec::new(),
        }
    }

    /// The thread's approval policy and sandbox, as Codex's answer to starting or resuming it
    /// says them: the policy is the thread's mode, the sandbox one of its facts.
    pub fn settings(
        &mut self,
        approval: p::AskForApproval,
        sandbox: &p::SandboxPolicy,
    ) -> Vec<Action> {
        let mode = match serde_json::to_value(approval) {
            Ok(Value::String(name)) => name,
            Ok(Value::Object(tagged)) => tagged.keys().next().cloned().unwrap_or_default(),
            _ => String::new(),
        };
        let sandbox = serde_json::to_value(sandbox)
            .ok()
            .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        let mut actions = Vec::new();
        if !sandbox.is_empty() && self.meta.facts.get("sandbox") != Some(&sandbox) {
            self.meta.facts.insert("sandbox".to_owned(), sandbox);
            actions.push(Action::Meta(Box::new(self.meta.clone())));
        }
        let mode = Some(mode).filter(|m| !m.is_empty());
        if mode != self.meters.mode {
            self.meters.mode = mode;
            actions.push(Action::MetersSet(self.meters.clone()));
        }
        actions
    }

    /// The account's rate-limit windows, which hold for every thread on it: Codex's primary
    /// and secondary windows, named by their length as Claude Code's are.
    pub fn rate_limits(&mut self, snapshot: &p::RateLimitSnapshot) -> Vec<Action> {
        let limits: Vec<Limit> = [snapshot.primary, snapshot.secondary]
            .into_iter()
            .flatten()
            .map(|window| Limit {
                name: window.window_duration_mins.map_or_else(|| "window".to_owned(), window_name),
                used_bp: u32::try_from(window.used_percent.clamp(0, 100))
                    .unwrap_or_default()
                    .saturating_mul(100),
                resets_ms: window.resets_at.map(seconds),
            })
            .collect();
        if limits == self.meters.limits {
            return Vec::new();
        }
        self.meters.limits = limits;
        vec![Action::MetersSet(self.meters.clone())]
    }

    /// What Codex estimates the thread has cost so far (`account/usage/read` for it). An account
    /// billed in credits alone has no figure in dollars, and keeps none.
    pub fn usage(&mut self, read: &p::GetAccountTokenUsageResponse) -> Vec<Action> {
        let cost = read
            .thread_usage
            .as_ref()
            .and_then(|usage| usage.estimated_usage_usd_micros)
            .and_then(|micros| u64::try_from(micros).ok());
        if cost.is_none() || cost == self.meters.cost_micro_usd {
            return Vec::new();
        }
        self.meters.cost_micro_usd = cost;
        vec![Action::MetersSet(self.meters.clone())]
    }

    /// The thread is a subagent started by the call `link` names.
    pub fn adopted(&mut self, link: Link) -> Vec<Action> {
        if self.meta.parent.as_ref() == Some(&link) {
            return Vec::new();
        }
        self.meta.parent = Some(link);
        ThreadMeta::SUBAGENT.clone_into(&mut self.meta.origin);
        vec![Action::Meta(Box::new(self.meta.clone()))]
    }

    /// The whole of what turn `codex` has changed so far, as Codex diffs it: the lines it
    /// added and removed.
    fn turn_diff(&mut self, codex: &str, diff: &str) -> Vec<Action> {
        let turn = self.turn(codex);
        let patch = patch_of(diff);
        let changed = Changed { added: patch.added, removed: patch.removed };
        match self.begun.get_mut(&turn) {
            Some(begun) if begun.changed != changed => {
                begun.changed = changed;
                vec![Action::TurnStarted(begun.clone())]
            }
            _ => Vec::new(),
        }
    }

    fn notice(&mut self, turn: TurnId, kind: &str, text: &str, now: WallMs) -> Vec<Action> {
        self.errors = self.errors.saturating_add(1);
        let item = Item {
            id: ItemId(format!("notice:{}", self.errors)),
            turn,
            at_ms: now,
            body: ItemBody::Notice(Notice::new(kind, Clipped::whole(text))),
        };
        vec![Action::ItemCompleted(item)]
    }

    /// The app-server's request `id` about this thread, heard at `now`.
    pub fn request(&mut self, id: &RequestId, request: &ServerRequest, now: WallMs) -> Vec<Action> {
        let ask = ask_of(id);
        let (item, kind, title, text, options, answers) = match request {
            ServerRequest::ItemCommandExecutionRequestApproval(asked) => {
                let command = unwrapped(asked.command.as_deref().unwrap_or_default());
                let title = if command.is_empty() {
                    "Run a command?".to_owned()
                } else {
                    format!("{}?", run_title(&command))
                };
                let decisions = asked.available_decisions.clone().unwrap_or_else(|| {
                    vec![
                        CommandExecutionApprovalDecision::Accept,
                        CommandExecutionApprovalDecision::Decline,
                    ]
                });
                let (options, answers) = command_choices(&decisions);
                (
                    Some(asked.item_id.clone()),
                    Request::APPROVAL,
                    title,
                    asked.reason.clone(),
                    options,
                    answers,
                )
            }
            ServerRequest::ItemFileChangeRequestApproval(asked) => {
                let decisions = [
                    FileChangeApprovalDecision::Accept,
                    FileChangeApprovalDecision::AcceptForSession,
                    FileChangeApprovalDecision::Decline,
                    FileChangeApprovalDecision::Cancel,
                ];
                let (options, answers) = file_choices(&decisions);
                let title = "Apply these edits?".to_owned();
                (
                    Some(asked.item_id.clone()),
                    Request::APPROVAL,
                    title,
                    asked.reason.clone(),
                    options,
                    answers,
                )
            }
            ServerRequest::ItemPermissionsRequestApproval(asked) => {
                let title = "Grant more permissions?".to_owned();
                (
                    Some(asked.item_id.clone()),
                    Request::APPROVAL,
                    title,
                    asked.reason.clone(),
                    Vec::new(),
                    BTreeMap::new(),
                )
            }
            ServerRequest::ItemToolRequestUserInput(asked) => {
                let title = match asked.questions.as_slice() {
                    [one] => one.question.clone(),
                    [] => "A question".to_owned(),
                    many => format!("{} questions", many.len()),
                };
                let text =
                    asked.questions.iter().any(|q| q.is_secret == Some(true)).then(|| {
                        "It asks for a secret: answer it in Codex's own terminal.".to_owned()
                    });
                (
                    Some(asked.item_id.clone()),
                    Request::QUESTION,
                    title,
                    text,
                    Vec::new(),
                    BTreeMap::new(),
                )
            }
            ServerRequest::McpServerElicitationRequest(asked) => {
                let title = format!("{} asks for input", asked.server_name);
                let form = Form::of(&asked.mode);
                let text =
                    form.as_ref().map(|f| f.message.clone()).filter(|m| !m.trim().is_empty());
                let options = if form.is_some() { form_choices() } else { Vec::new() };
                (None, Request::ELICITATION, title, text, options, BTreeMap::new())
            }
        };
        let form = match request {
            ServerRequest::McpServerElicitationRequest(asked) => Form::of(&asked.mode),
            _ => None,
        };
        let item = item.map(ItemId);
        let open_item = item.clone();
        // A secret is never asked here: an answer is kept in the thread's log, so the person
        // gives it in Codex's own terminal, which keeps it out of sight.
        let questions = match request {
            ServerRequest::ItemToolRequestUserInput(asked)
                if !asked.questions.iter().any(|q| q.is_secret == Some(true)) =>
            {
                asked.questions.iter().map(|q| (q.id.clone(), question(q))).collect()
            }
            _ => Vec::new(),
        };
        let shown: Vec<Question> = match &form {
            Some(form) => form.questions(),
            None => questions.iter().map(|(_, q)| q.clone()).collect(),
        };
        let open = Open {
            id: id.clone(),
            item: item.clone(),
            title: title.clone(),
            kind,
            answers,
            questions,
            form,
            answered: None,
        };
        self.open.insert(ask.clone(), open);
        // Codex may say the thread waits before it says on what, or not say it waits at all,
        // or not yet say it is active: the wait is worded again now, from what it asks.
        let waiting = match self.said.clone() {
            Some((status @ ThreadStatus::Active { .. }, since)) => Some((status, since)),
            None | Some((ThreadStatus::Idle, _)) => {
                Some((ThreadStatus::Active { active_flags: Vec::new() }, now))
            }
            Some((ThreadStatus::NotLoaded | ThreadStatus::SystemError, _)) => None,
        };
        let request = Request {
            editable: Vec::new(),
            id: ask,
            item,
            kind: kind.to_owned(),
            title,
            text: text.as_deref().map(Clipped::whole),
            options,
            questions: shown,
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: None,
        };
        let mut actions = vec![Action::RequestOpened(Box::new(request))];
        if let Some(item) = &open_item {
            actions.extend(self.restate(item));
        }
        if let Some((status, since)) = waiting {
            actions.push(self.status(status, since));
        }
        actions
    }

    /// The answer to request `ask` with choice `choice`, given by `by`: the app-server's
    /// request id and the result to send it. `None` when no such request is open here, or it
    /// offers no such choice, or the choice does not answer each of its questions.
    ///
    /// A question's choice is one answer to them all ([`Answer::read`]); each goes to Codex
    /// under its question's id, its picks and one's own words apart ([`Answer::parts`]).
    pub fn answer(
        &mut self,
        ask: &AskId,
        choice: &str,
        by: Answerer,
    ) -> Option<(RequestId, Value)> {
        let open = self.open.get_mut(ask)?;
        let result = if let Some(form) = &open.form {
            let response = match choice {
                DECLINE => p::McpServerElicitationRequestResponse {
                    meta: None,
                    action: p::McpServerElicitationAction::Decline,
                    content: None,
                },
                CANCEL => p::McpServerElicitationRequestResponse {
                    meta: None,
                    action: p::McpServerElicitationAction::Cancel,
                    content: None,
                },
                answers => p::McpServerElicitationRequestResponse {
                    meta: None,
                    action: p::McpServerElicitationAction::Accept,
                    content: Some(form.content(answers)?),
                },
            };
            serde_json::to_value(response).ok()?
        } else if open.questions.is_empty() {
            serde_json::json!({ "decision": open.answers.get(choice)?.clone() })
        } else {
            let asked: Vec<Question> = open.questions.iter().map(|(_, q)| q.clone()).collect();
            let given = Answer::read(&asked, choice)?;
            let answers = open
                .questions
                .iter()
                .zip(given)
                .map(|((id, q), a)| {
                    (id.clone(), p::ToolRequestUserInputAnswer { answers: a.parts(q) })
                })
                .collect();
            serde_json::to_value(p::ToolRequestUserInputResponse { answers }).ok()?
        };
        open.answered = Some((by, choice.to_owned()));
        Some((open.id.clone(), result))
    }

    /// What goes to the app-server for `text` and the files `attached`, sent as intent `intent`
    /// with `delivery`: into the turn under way when it steers and one is, else a turn of its
    /// own. A picture goes as Codex's `localImage`, which Codex reads from its path; any other
    /// file goes by its path in the words. A message queued while a turn runs is held here until
    /// it ends ([`Self::next_queued`]).
    pub fn send(
        &mut self,
        text: &str,
        attached: Vec<Attached>,
        delivery: Delivery,
        intent: IntentId,
    ) -> Send {
        if delivery == Delivery::Queue && self.current.is_some() {
            let pending = Pending {
                intent,
                text: text.to_owned(),
                attachments: attached.iter().map(|a| a.path().to_owned()).collect(),
                delivery,
                state: PendingState::Waiting,
            };
            self.queued.push_back((pending, attached));
            return Send::Held(vec![self.pending_now()]);
        }
        let input = input(text, &attached);
        let client = Some(intent.to_string());
        match (&self.current, delivery) {
            (Some(turn), Delivery::Steer) => Send::Steer(Box::new(p::TurnSteerParams {
                additional_context: None,
                client_user_message_id: client,
                expected_turn_id: turn.clone(),
                input,
                responsesapi_client_metadata: None,
                thread_id: self.meta.native.clone(),
            })),
            _ => Send::Start(Box::new(p::TurnStartParams {
                client_user_message_id: client,
                input,
                thread_id: self.meta.native.clone(),
                ..p::TurnStartParams::default()
            })),
        }
    }

    /// Take back the message held for `intent`; `None` when none is.
    pub fn withdraw(&mut self, intent: IntentId) -> Option<Vec<Action>> {
        let at = self.queued.iter().position(|(p, _)| p.intent == intent)?;
        self.queued.remove(at);
        Some(vec![self.pending_now()])
    }

    /// Make the message held for `intent` say `text`, its files kept; `None` when none is.
    pub fn edit(&mut self, intent: IntentId, text: &str) -> Option<Vec<Action>> {
        let (held, _) = self.queued.iter_mut().find(|(p, _)| p.intent == intent)?;
        text.clone_into(&mut held.text);
        Some(vec![self.pending_now()])
    }

    /// The message held for `intent`, sent now: into the turn under way as a steer, or as a
    /// turn of its own when none is. What goes to the app-server and the actions that take it
    /// off the queue; `None` when none is held.
    pub fn promote(&mut self, intent: IntentId) -> Option<(Send, Vec<Action>)> {
        let at = self.queued.iter().position(|(p, _)| p.intent == intent)?;
        let (held, attached) = self.queued.remove(at)?;
        let send = self.send(&held.text, attached, Delivery::Steer, intent);
        Some((send, vec![self.pending_now()]))
    }

    /// Move the message held for `intent` to just before the one held for `before`, or to the
    /// end; `None` when either is not held.
    pub fn reorder(&mut self, intent: IntentId, before: Option<IntentId>) -> Option<Vec<Action>> {
        let queued = self.queued.make_contiguous();
        Pending::reorder(queued, |(p, _)| p.intent, intent, before)
            .then(|| vec![self.pending_now()])
    }

    /// Whether the thread rests: no turn under way, nothing asked of the person, no message
    /// held, and Codex not saying it works on it.
    #[must_use]
    pub fn rests(&self) -> bool {
        self.current.is_none()
            && self.open.is_empty()
            && self.queued.is_empty()
            && !matches!(self.said, Some((ThreadStatus::Active { .. }, _)))
    }

    /// The next message held, as the turn that sends it and the actions that take it off the
    /// queue, once no turn is under way.
    pub fn next_queued(&mut self) -> Option<(Box<p::TurnStartParams>, Vec<Action>)> {
        if self.current.is_some() {
            return None;
        }
        let (next, attached) = self.queued.pop_front()?;
        let Send::Start(params) = self.send(&next.text, attached, Delivery::Steer, next.intent)
        else {
            return None;
        };
        Some((params, vec![self.pending_now()]))
    }

    fn pending_now(&self) -> Action {
        Action::PendingSet(self.queued.iter().map(|(p, _)| p.clone()).collect())
    }

    /// What branches a new thread off this one, sharing its turns through `after`, or all of
    /// them (`thread/fork`); why not, in words, when it cannot.
    ///
    /// # Errors
    ///
    /// When `after` is no turn Codex holds of the thread, or the turn under way.
    pub fn fork(&self, after: Option<TurnId>) -> Result<p::ThreadForkParams, String> {
        let last = match after {
            None => None,
            Some(after) => {
                let codex = self.turns.iter().find(|(_, t)| **t == after).map(|(c, _)| c);
                let codex = codex.ok_or_else(|| format!("There is no turn {} here", after.0))?;
                if self.current.as_ref() == Some(codex) {
                    return Err("That turn is still under way".to_owned());
                }
                Some(codex.clone())
            }
        };
        Ok(p::ThreadForkParams {
            thread_id: self.meta.native.clone(),
            last_turn_id: last,
            config: Some(unclaimed()),
            ..p::ThreadForkParams::default()
        })
    }

    /// What branches a new thread off this one with the turns before `turn` and none from it on
    /// (`thread/fork` with `beforeTurnId`); why not, in words, when it cannot.
    ///
    /// # Errors
    ///
    /// When `turn` is no turn Codex holds of the thread.
    pub fn fork_before(&self, turn: TurnId) -> Result<p::ThreadForkParams, String> {
        let codex = self.turns.iter().find(|(_, t)| **t == turn).map(|(c, _)| c);
        let codex = codex.ok_or_else(|| format!("There is no turn {} here", turn.0))?;
        Ok(p::ThreadForkParams {
            thread_id: self.meta.native.clone(),
            before_turn_id: Some(codex.clone()),
            config: Some(unclaimed()),
            ..p::ThreadForkParams::default()
        })
    }

    /// The terminal Codex's own TUI runs in on this thread, opened by the worker, or none once
    /// it closed: the thread names it, so a client can show it.
    pub fn set_terminal(&mut self, terminal: Option<SessionId>) -> Vec<Action> {
        if self.meta.terminal == terminal {
            return Vec::new();
        }
        self.meta.terminal = terminal;
        vec![Action::Meta(Box::new(self.meta.clone()))]
    }

    /// What stops the turn under way, when one is.
    #[must_use]
    pub fn interrupt(&self) -> Option<p::TurnInterruptParams> {
        let turn = self.current.clone()?;
        Some(p::TurnInterruptParams { thread_id: self.meta.native.clone(), turn_id: turn })
    }

    fn turn(&mut self, codex: &str) -> TurnId {
        if let Some(turn) = self.turns.get(codex) {
            return *turn;
        }
        let next = u32::try_from(self.turns.len()).unwrap_or(u32::MAX).saturating_add(1);
        let turn = TurnId(next);
        self.turns.insert(codex.to_owned(), turn);
        turn
    }

    fn turn_started(&mut self, turn: &p::Turn) -> Vec<Action> {
        let id = self.turn(&turn.id);
        if matches!(turn.status, TurnStatus::InProgress) {
            self.current = Some(turn.id.clone());
        }
        let begun = Turn {
            id,
            input: None,
            state: TurnState::Active,
            started_ms: turn.started_at.map_or(WallMs::ZERO, seconds),
            ended_ms: None,
            usage: Usage::default(),
            models: self.meters.model.iter().cloned().collect(),
            changed: Changed::default(),
            before: None,
            after: None,
        };
        self.begun.insert(id, begun.clone());
        vec![Action::TurnStarted(begun)]
    }

    fn turn_completed(&mut self, turn: &p::Turn) -> Vec<Action> {
        let id = self.turn(&turn.id);
        if self.current.as_deref() == Some(&turn.id) {
            self.current = None;
        }
        let state = match (&turn.status, &turn.error) {
            (TurnStatus::Interrupted, _) => TurnState::Interrupted,
            (TurnStatus::Failed, error) => TurnState::Failed {
                error: error.as_ref().map_or_else(String::new, |e| e.message.clone()),
                until_ms: error
                    .as_ref()
                    .filter(|e| limited(e.codex_error_info))
                    .and_then(|_| crate::driven::reset_of_full(&self.meters.limits)),
            },
            _ => TurnState::Complete,
        };
        self.last_end = Some(state.clone());
        let mut actions = Vec::new();
        // Whatever the turn still asked is no longer asked.
        let gone: Vec<AskId> = self.open.keys().cloned().collect();
        for ask in gone {
            let item = self.open.remove(&ask).and_then(|open| open.item);
            actions.push(Action::RequestResolved { id: ask, state: RequestState::Withdrawn });
            actions.extend(item.and_then(|item| self.restate(&item)));
        }
        actions.push(Action::TurnEnded {
            turn: id,
            state,
            usage: std::mem::take(&mut self.usage),
            ended_ms: turn.completed_at.map_or(WallMs::ZERO, seconds),
        });
        actions
    }

    fn resolved(&mut self, id: &RequestId) -> Vec<Action> {
        let ask = ask_of(id);
        let Some(open) = self.open.remove(&ask) else { return Vec::new() };
        let (by, choice) = open.answered.unwrap_or_else(|| {
            (Answerer { client: None, name: ELSEWHERE.to_owned() }, String::new())
        });
        let mut actions =
            vec![Action::RequestResolved { id: ask, state: RequestState::Answered { by, choice } }];
        actions.extend(open.item.and_then(|item| self.restate(&item)));
        actions
    }

    /// Call `item` told again with the state the requests open now give it: waiting on the
    /// person while one is about it, else running. Nothing for a call that is whole, or not
    /// begun.
    fn restate(&mut self, item: &ItemId) -> Option<Action> {
        let state = self.pending(&item.0).unwrap_or(ToolState::Running);
        let shown = self.calls.get_mut(item)?;
        let ItemBody::Tool(call) = &mut shown.body else { return None };
        if call.state == state {
            return None;
        }
        call.state = state;
        Some(Action::ItemUpdated(shown.clone()))
    }

    /// The thread's status as Codex says it, from `since`.
    fn status(&mut self, status: ThreadStatus, since: WallMs) -> Action {
        let mapped = self.status_of(&status, since);
        self.said = Some((status, since));
        Action::Status(mapped)
    }

    fn status_of(&self, status: &ThreadStatus, now: WallMs) -> Status {
        let (phase, wait, liveness) = match status {
            ThreadStatus::NotLoaded => (Phase::Idle, None, Liveness::Exited { resumable: true }),
            ThreadStatus::SystemError => (Phase::Failed, None, Liveness::Live),
            ThreadStatus::Idle => {
                let phase = match &self.last_end {
                    Some(TurnState::Complete) => Phase::Done,
                    Some(TurnState::Failed { .. }) => Phase::Failed,
                    Some(TurnState::Interrupted) => Phase::Stopped,
                    Some(TurnState::Active) | None => Phase::Idle,
                };
                (phase, None, Liveness::Live)
            }
            // A request open here waits on the person whatever flags Codex sent with it.
            ThreadStatus::Active { active_flags } => {
                let open = self.open.values().next();
                let approval = open.map_or_else(
                    || active_flags.contains(&ThreadActiveFlag::WaitingOnApproval),
                    |open| open.kind == Request::APPROVAL,
                );
                let asks =
                    open.is_some() || active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput);
                let what = open.map(|open| open.title.clone());
                if approval {
                    let text = what.unwrap_or_else(|| "Waits for approval".to_owned());
                    (
                        Phase::NeedsYou,
                        Some(Wait { kind: "permission".to_owned(), text }),
                        Liveness::Live,
                    )
                } else if asks {
                    let text = what.unwrap_or_else(|| "Has a question".to_owned());
                    (
                        Phase::NeedsYou,
                        Some(Wait { kind: "question".to_owned(), text }),
                        Liveness::Live,
                    )
                } else {
                    (Phase::Working, None, Liveness::Live)
                }
            }
        };
        Status { phase, wait, liveness, since_ms: now }
    }

    /// Item `item` of Codex turn `turn`, begun or (`whole`) completed at `at`.
    fn item(&mut self, turn: &str, item: &ThreadItem, at: WallMs, whole: bool) -> Vec<Action> {
        let turn = self.turn(turn);
        let Some((id, body)) = self.body(item) else { return Vec::new() };
        let mut actions = Vec::new();
        // The person's message is what started its turn.
        if matches!(body, ItemBody::User(_))
            && let Some(begun) = self.begun.get_mut(&turn)
            && begun.input.is_none()
        {
            begun.input = Some(id.clone());
            actions.push(Action::TurnStarted(begun.clone()));
        }
        // A thread Codex has not named is named by what the person first said in it.
        if let ItemBody::User(message) = &body
            && self.meta.title.is_empty()
            && !message.text.text.trim().is_empty()
        {
            self.meta.title = crate::driven::title_of(&message.text.text);
            actions.push(Action::Meta(Box::new(self.meta.clone())));
        }
        let item = Item { id, turn, at_ms: at, body };
        if whole {
            self.calls.remove(&item.id);
        } else if matches!(item.body, ItemBody::Tool(_)) {
            self.calls.insert(item.id.clone(), item.clone());
        }
        actions.push(if whole { Action::ItemCompleted(item) } else { Action::ItemStarted(item) });
        actions
    }

    fn body(&self, item: &ThreadItem) -> Option<(ItemId, ItemBody)> {
        let (id, body) = match item {
            ThreadItem::UserMessage { client_id, content, id } => {
                let text: Vec<&str> = content
                    .iter()
                    .filter_map(|input| match input {
                        UserInput::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let intent = client_id.as_deref().and_then(|c| c.parse().ok());
                let message = UserMessage {
                    text: Clipped::head(&text.join("\n"), PROSE, None),
                    images: Vec::new(),
                    command: None,
                    intent,
                };
                (id, ItemBody::User(message))
            }
            ThreadItem::AgentMessage { id, text, .. } => {
                (id, ItemBody::Text(Clipped::head(text, PROSE, None)))
            }
            ThreadItem::Reasoning { id, summary, content } => {
                let text = summary
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .or(content.as_ref())
                    .map(|parts| parts.join("\n\n"))
                    .unwrap_or_default();
                (id, ItemBody::Reasoning(Clipped::head(&text, PROSE, None)))
            }
            ThreadItem::CommandExecution {
                aggregated_output,
                command,
                cwd,
                duration_ms,
                exit_code,
                id,
                status,
                ..
            } => {
                let state = self.pending(id).unwrap_or(match status {
                    CommandExecutionStatus::InProgress => ToolState::Running,
                    CommandExecutionStatus::Completed => ToolState::Completed,
                    CommandExecutionStatus::Failed => ToolState::Failed,
                    CommandExecutionStatus::Declined => ToolState::Rejected,
                });
                let run = unwrapped(command);
                let exec = ExecDetail {
                    command: Clipped::whole(&run),
                    description: None,
                    cwd: Some(cwd.clone()),
                    background: false,
                    task: None,
                    status: match status {
                        CommandExecutionStatus::InProgress => ExecStatus::Running,
                        CommandExecutionStatus::Completed => ExecStatus::Done,
                        CommandExecutionStatus::Failed => ExecStatus::Failed,
                        CommandExecutionStatus::Declined => ExecStatus::Interrupted,
                    },
                    exit_code: *exit_code,
                    stderr: None,
                    duration_ms: duration_ms.and_then(|d| u64::try_from(d).ok()),
                };
                let call = ToolCall {
                    name: "commandExecution".to_owned(),
                    kind: kind::EXEC.to_owned(),
                    title: run_title(&run),
                    input: Clipped::whole(&serde_json::json!({ "command": command }).to_string()),
                    state,
                    output: aggregated_output.as_deref().map(|o| Clipped::tail(o, OUTPUT, None)),
                    images: Vec::new(),
                    detail: Some(ToolDetail::Exec(exec)),
                    child: None,
                    ended_ms: None,
                };
                (id, ItemBody::Tool(Box::new(call)))
            }
            ThreadItem::FileChange { changes, id, status } => {
                let state = self.pending(id).unwrap_or(match status {
                    PatchApplyStatus::InProgress => ToolState::Running,
                    PatchApplyStatus::Completed => ToolState::Completed,
                    PatchApplyStatus::Failed => ToolState::Failed,
                    PatchApplyStatus::Declined => ToolState::Rejected,
                });
                let title = match changes.as_slice() {
                    [one] => format!("Edit {}", one.path),
                    many => format!("Edit {} files", many.len()),
                };
                let detail = changes.first().filter(|_| changes.len() == 1).map(|change| {
                    ToolDetail::Edit(EditDetail {
                        path: change.path.clone(),
                        edits: 1,
                        replace_all: false,
                        patch: patch_of(&change.diff),
                    })
                });
                let input = serde_json::to_string(changes).unwrap_or_default();
                let call = ToolCall {
                    name: "fileChange".to_owned(),
                    kind: kind::EDIT.to_owned(),
                    title,
                    input: Clipped::head(&input, OUTPUT, None),
                    state,
                    output: None,
                    images: Vec::new(),
                    detail,
                    child: None,
                    ended_ms: None,
                };
                (id, ItemBody::Tool(Box::new(call)))
            }
            ThreadItem::McpToolCall {
                arguments, error, id, result, server, status, tool, ..
            } => {
                let state = self.pending(id).unwrap_or(match status {
                    p::McpToolCallStatus::InProgress => ToolState::Running,
                    p::McpToolCallStatus::Completed => ToolState::Completed,
                    p::McpToolCallStatus::Failed => ToolState::Failed,
                });
                let output = match (result, error) {
                    (_, Some(error)) => Some(error.message.clone()),
                    (Some(result), None) => serde_json::to_string(&result.content).ok(),
                    (None, None) => None,
                };
                let call = ToolCall {
                    name: format!("{server}.{tool}"),
                    kind: kind::MCP.to_owned(),
                    title: format!("{server}: {tool}"),
                    input: Clipped::head(&arguments.to_string(), OUTPUT, None),
                    state,
                    output: output.as_deref().map(|o| Clipped::head(o, OUTPUT, None)),
                    images: Vec::new(),
                    detail: Some(ToolDetail::Mcp(McpDetail {
                        server: server.clone(),
                        tool: tool.clone(),
                    })),
                    child: None,
                    ended_ms: None,
                };
                (id, ItemBody::Tool(Box::new(call)))
            }
            ThreadItem::WebSearch { id, query, .. } => {
                let call = ToolCall {
                    name: "webSearch".to_owned(),
                    kind: kind::WEB_SEARCH.to_owned(),
                    title: query.clone(),
                    input: Clipped::whole(&serde_json::json!({ "query": query }).to_string()),
                    state: ToolState::Completed,
                    output: None,
                    images: Vec::new(),
                    detail: None,
                    child: None,
                    ended_ms: None,
                };
                (id, ItemBody::Tool(Box::new(call)))
            }
            ThreadItem::Plan { id, text } => {
                let call = ToolCall {
                    name: "plan".to_owned(),
                    kind: kind::PLAN.to_owned(),
                    title: "Plan".to_owned(),
                    input: Clipped::whole(""),
                    state: ToolState::Completed,
                    output: None,
                    images: Vec::new(),
                    detail: Some(ToolDetail::Plan { text: Clipped::head(text, PROSE, None) }),
                    child: None,
                    ended_ms: None,
                };
                (id, ItemBody::Tool(Box::new(call)))
            }
            ThreadItem::ContextCompaction { id } => {
                let compaction = Compaction {
                    trigger: None,
                    before_tokens: None,
                    after_tokens: None,
                    summary: None,
                };
                (id, ItemBody::Compaction(compaction))
            }
            ThreadItem::CollabAgentToolCall {
                agents_states,
                id,
                model,
                prompt,
                receiver_thread_ids,
                status,
                tool,
                ..
            } => {
                let state = self.pending(id).unwrap_or(match status {
                    p::CollabAgentToolCallStatus::InProgress => ToolState::Running,
                    p::CollabAgentToolCallStatus::Completed => ToolState::Completed,
                    p::CollabAgentToolCallStatus::Failed => ToolState::Failed,
                    p::CollabAgentToolCallStatus::Interrupted => ToolState::Cancelled,
                });
                let title = match tool {
                    p::CollabAgentTool::SpawnAgent => "Start a subagent",
                    p::CollabAgentTool::SendInput | p::CollabAgentTool::SendMessage => {
                        "Message a subagent"
                    }
                    p::CollabAgentTool::FollowupTask => "Give a subagent more to do",
                    p::CollabAgentTool::ResumeAgent => "Resume a subagent",
                    p::CollabAgentTool::Wait => "Wait for subagents",
                    p::CollabAgentTool::InterruptAgent => "Interrupt a subagent",
                    p::CollabAgentTool::CloseAgent => "Close a subagent",
                    p::CollabAgentTool::ListAgents => "List the subagents",
                };
                // What the subagents said, by thread, as the call last knew it.
                let said: Vec<&str> =
                    agents_states.values().filter_map(|s| s.message.as_deref()).collect();
                let report =
                    (!said.is_empty()).then(|| Clipped::head(&said.join("\n\n"), PROSE, None));
                let prompt = prompt.as_deref().unwrap_or_default();
                let call = ToolCall {
                    name: format!("collabAgentToolCall.{}", wire(tool)),
                    kind: kind::AGENT.to_owned(),
                    title: title.to_owned(),
                    input: Clipped::head(
                        &serde_json::json!({ "prompt": prompt, "model": model }).to_string(),
                        OUTPUT,
                        None,
                    ),
                    state,
                    output: None,
                    images: Vec::new(),
                    detail: Some(ToolDetail::Agent(AgentDetail {
                        agent_type: model.clone(),
                        description: None,
                        prompt: Clipped::head(prompt, PROSE, None),
                        background: false,
                        report,
                        tokens: None,
                        tool_uses: None,
                        duration_ms: None,
                    })),
                    child: receiver_thread_ids
                        .first()
                        .filter(|_| *tool == p::CollabAgentTool::SpawnAgent)
                        .map(|native| thread_of(native)),
                    ended_ms: None,
                };
                (id, ItemBody::Tool(Box::new(call)))
            }
            ThreadItem::EnteredReviewMode { id, .. } => (id, ItemBody::Review { entered: true }),
            ThreadItem::ExitedReviewMode { id, .. } => (id, ItemBody::Review { entered: false }),
            other => {
                let json = serde_json::to_value(other).ok()?;
                let id = json.get("id").and_then(Value::as_str)?.to_owned();
                let kind = json.get("type").and_then(Value::as_str).unwrap_or("item").to_owned();
                let body =
                    ItemBody::Extra { kind, json: Clipped::head(&json.to_string(), OUTPUT, None) };
                return Some((ItemId(id), body));
            }
        };
        Some((ItemId(id.clone()), body))
    }

    /// The request a call waits on, while one is open for it.
    fn pending(&self, item: &str) -> Option<ToolState> {
        self.open
            .iter()
            .find(|(_, open)| open.item.as_ref().is_some_and(|i| i.0 == item))
            .map(|(ask, _)| ToolState::Pending { ask: ask.clone() })
    }
}

fn append(item: &str, part: PartKey, text: &str) -> Vec<Action> {
    vec![Action::Append { item: ItemId(item.to_owned()), part, text: text.to_owned() }]
}

/// The [`AskId`] of Codex request `id`.
#[must_use]
pub fn ask_of(id: &RequestId) -> AskId {
    match id {
        RequestId::Integer(n) => AskId(n.to_string()),
        RequestId::String(s) => AskId(s.clone()),
    }
}

/// A thread's name for people: its own, else what it was first asked.
/// What a message of `text` and the files `attached` is to Codex: the words, with the paths of
/// the files that are no pictures after them, then each picture as a `localImage`.
fn input(text: &str, attached: &[Attached]) -> Vec<UserInput> {
    let words = crate::attach::with_files(text, attached);
    let words = (!words.trim().is_empty())
        .then(|| UserInput::Text { text: words, text_elements: Some(Vec::new()) });
    let pictures = crate::attach::pictures(attached)
        .map(|picture| UserInput::LocalImage { detail: None, path: picture.path().to_owned() });
    words.into_iter().chain(pictures).collect()
}

/// What lists Codex's threads in folder `cwd`, most recently changed first, at most `limit`
/// (`thread/list`).
#[must_use]
pub fn list(cwd: &str, limit: u32) -> p::ThreadListParams {
    p::ThreadListParams {
        cwd: Some(p::ThreadListCwdFilter::String(cwd.to_owned())),
        limit: Some(limit),
        sort_key: Some(p::ThreadSortKey::UpdatedAt),
        ..p::ThreadListParams::default()
    }
}

/// One of the threads `thread/list` listed, as a past session: taken up again by
/// [`resume_args`].
#[must_use]
pub fn past(thread: &p::Thread) -> PastSession {
    let mut facts = BTreeMap::new();
    if let Some(branch) = thread.git_info.as_ref().and_then(|g| g.branch.clone()) {
        facts.insert("branch".to_owned(), branch);
    }
    if let Some(model) = thread.model.clone().filter(|m| !m.is_empty()) {
        facts.insert("model".to_owned(), model);
    }
    if let Some(from) = thread.forked_from_id.clone().filter(|f| !f.is_empty()) {
        facts.insert("forked-from".to_owned(), from);
    }
    let title = title_of(thread);
    PastSession {
        agent: AgentId::named(AgentId::CODEX),
        native: thread.id.clone(),
        cwd: Some(thread.cwd.clone()),
        prompts: Vec::new(),
        title: (!title.trim().is_empty()).then_some(title),
        updated_ms: Some(seconds(thread.updated_at)),
        thread: None,
        resume: resume_args(&thread.id),
        facts,
    }
}

fn title_of(thread: &p::Thread) -> String {
    thread
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| thread.preview.lines().next().unwrap_or_default().to_owned())
}

/// Whether Codex says a turn stopped on the account's usage or rate limit.
const fn limited(info: Option<p::CodexErrorInfo>) -> bool {
    matches!(
        info,
        Some(p::CodexErrorInfo::UsageLimitExceeded | p::CodexErrorInfo::RateLimitExceeded)
    )
}

/// A call's title for command `run`: "Run" and its first line, as every agent's command reads.
fn run_title(run: &str) -> String {
    format!("Run {}", run.lines().next().unwrap_or_default().trim())
}

/// The command a shell runs for Codex: `/bin/zsh -lc 'cargo test'` is `cargo test`. Codex runs
/// each command through the person's shell, and the wrapper says nothing to them. Any other
/// command is as it was.
fn unwrapped(command: &str) -> String {
    let inner = command.trim().split_once(char::is_whitespace).and_then(|(shell, rest)| {
        let shell = shell.rsplit('/').next().unwrap_or_default();
        let (flag, script) = rest.trim_start().split_once(char::is_whitespace)?;
        let runs = flag.len() > 1
            && flag.starts_with('-')
            && flag.ends_with('c')
            && flag.chars().skip(1).all(|c| c.is_ascii_lowercase());
        (SHELLS.contains(&shell) && runs).then(|| one_word(script.trim())).flatten()
    });
    inner.unwrap_or_else(|| command.to_owned())
}

/// The shells Codex wraps a command in.
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// `text` as the one word a POSIX shell reads it as, its quotes taken off; `None` when it is
/// more than one word, or a quote is left open.
fn one_word(text: &str) -> Option<String> {
    let mut word = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    c => word.push(c),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => {
                        let next = chars.next()?;
                        if !matches!(next, '"' | '\\' | '$' | '`' | '\n') {
                            word.push('\\');
                        }
                        if next != '\n' {
                            word.push(next);
                        }
                    }
                    c => word.push(c),
                }
            },
            '\\' => word.push(chars.next()?),
            c if c.is_whitespace() => return None,
            c => word.push(c),
        }
    }
    Some(word)
}

/// A Unix time in seconds, as Codex writes a thread's and a turn's times.
fn seconds(at: i64) -> WallMs {
    WallMs::from_millis(u64::try_from(at).unwrap_or_default().saturating_mul(1_000))
}

/// A Unix time in milliseconds, as Codex writes an item's.
fn millis(at: i64) -> WallMs {
    WallMs::from_millis(u64::try_from(at).unwrap_or_default())
}

/// `value`'s name on the wire: a string enum's value.
fn wire<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        _ => String::new(),
    }
}

fn usage_of(tokens: &p::TokenUsageBreakdown) -> Usage {
    let mut usage = BTreeMap::new();
    let mut put = |kind: &str, n: i64| {
        if let Ok(n) = u64::try_from(n)
            && n > 0
        {
            usage.insert(kind.to_owned(), n);
        }
    };
    put(Usage::INPUT, tokens.input_tokens);
    put(Usage::CACHE_READ, tokens.cached_input_tokens);
    put(Usage::CACHE_WRITE, tokens.cache_write_input_tokens.unwrap_or_default());
    put(Usage::OUTPUT, tokens.output_tokens);
    put(Usage::REASONING, tokens.reasoning_output_tokens);
    Usage(usage)
}

/// A form's answers beside its questions: decline it, or cancel it and stop the turn.
fn form_choices() -> Vec<Choice> {
    let choice = |id: &str, label: &str, stops| Choice {
        id: id.to_owned(),
        label: label.to_owned(),
        effect: Effect::Deny,
        scope: None,
        stops,
    };
    vec![choice(DECLINE, "Decline", false), choice(CANCEL, "Cancel", true)]
}

/// A command approval's choices, in Codex's order, and the decision each sends.
fn command_choices(
    decisions: &[CommandExecutionApprovalDecision],
) -> (Vec<Choice>, BTreeMap<String, Value>) {
    let mut options = Vec::new();
    let mut answers = BTreeMap::new();
    for decision in decisions {
        let (label, effect, scope, stops) = match decision {
            CommandExecutionApprovalDecision::Accept => ("Allow", Effect::Allow, None, false),
            CommandExecutionApprovalDecision::AcceptForSession => {
                ("Allow for this session", Effect::Allow, Some("this session".to_owned()), false)
            }
            CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment(rule) => {
                ("Always allow", Effect::Allow, Some(rule.execpolicy_amendment.join(" ")), false)
            }
            CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment(rule) => {
                let scope = serde_json::to_value(&rule.network_policy_amendment)
                    .ok()
                    .and_then(|v| v.get("host").and_then(Value::as_str).map(str::to_owned));
                ("Apply the network rule", Effect::Allow, scope, false)
            }
            CommandExecutionApprovalDecision::Decline => ("Deny", Effect::Deny, None, false),
            CommandExecutionApprovalDecision::Cancel => ("Deny and stop", Effect::Deny, None, true),
        };
        push_choice(&mut options, &mut answers, decision, (label, effect, scope, stops));
    }
    (options, answers)
}

/// A file change approval's choices, and the decision each sends.
fn file_choices(
    decisions: &[FileChangeApprovalDecision],
) -> (Vec<Choice>, BTreeMap<String, Value>) {
    let mut options = Vec::new();
    let mut answers = BTreeMap::new();
    for decision in decisions {
        let (label, effect, scope, stops) = match decision {
            FileChangeApprovalDecision::Accept => ("Allow", Effect::Allow, None, false),
            FileChangeApprovalDecision::AcceptForSession => {
                ("Allow for this session", Effect::Allow, Some("this session".to_owned()), false)
            }
            FileChangeApprovalDecision::Decline => ("Deny", Effect::Deny, None, false),
            FileChangeApprovalDecision::Cancel => ("Deny and stop", Effect::Deny, None, true),
        };
        push_choice(&mut options, &mut answers, decision, (label, effect, scope, stops));
    }
    (options, answers)
}

/// `decision` as a choice, its id the decision's own name on the wire.
fn push_choice<T: serde::Serialize>(
    options: &mut Vec<Choice>,
    answers: &mut BTreeMap<String, Value>,
    decision: &T,
    (label, effect, scope, stops): (&str, Effect, Option<String>, bool),
) {
    let Ok(value) = serde_json::to_value(decision) else { return };
    let id = match &value {
        Value::String(name) => name.clone(),
        Value::Object(one) => one.keys().next().cloned().unwrap_or_default(),
        _ => return,
    };
    options.push(Choice { id: id.clone(), label: label.to_owned(), effect, scope, stops });
    answers.insert(id, value);
}

/// A unified diff's hunks, as Codex writes a file change's.
fn patch_of(diff: &str) -> Patch {
    let mut patch = Patch::default();
    for line in diff.lines() {
        if let Some(head) = line.strip_prefix("@@ ") {
            let mut ranges = head.split(' ');
            let old = ranges.next().and_then(|r| r.strip_prefix('-')).map(range);
            let new = ranges.next().and_then(|r| r.strip_prefix('+')).map(range);
            let ((old_start, old_lines), (new_start, new_lines)) =
                (old.unwrap_or_default(), new.unwrap_or_default());
            patch.hunks.push(Hunk {
                old_start,
                old_lines,
                new_start,
                new_lines,
                heading: header_heading(line),
                lines: Vec::new(),
            });
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let Some(hunk) = patch.hunks.last_mut() else { continue };
        if line.starts_with('+') {
            patch.added = patch.added.saturating_add(1);
        } else if line.starts_with('-') {
            patch.removed = patch.removed.saturating_add(1);
        }
        hunk.lines.push(line.to_owned());
    }
    patch
}

/// A rate-limit window's name by its length: `five-hour`, `seven-day`, else in minutes.
fn window_name(minutes: i64) -> String {
    match minutes {
        300 => "five-hour".to_owned(),
        10_080 => "seven-day".to_owned(),
        m if m % 1_440 == 0 => format!("{}-day", m / 1_440),
        m if m % 60 == 0 => format!("{}-hour", m / 60),
        m => format!("{m}-minute"),
    }
}

/// `start,lines` of a hunk header; one line when it says no count.
fn range(text: &str) -> (u32, u32) {
    let mut parts = text.split(',');
    let start = parts.next().and_then(|s| s.parse().ok()).unwrap_or_default();
    let lines = parts.next().map_or(1, |s| s.parse().unwrap_or_default());
    (start, lines)
}

/// A Codex question as the thread asks it. Codex offers one answer of those it lists, and an
/// answer of one's own beside them where it says so; the card always has a field for one's own.
fn question(asked: &p::ToolRequestUserInputQuestion) -> Question {
    let options = asked.options.iter().flatten().map(|o| Offered {
        label: o.label.clone(),
        description: Some(o.description.clone()).filter(|d| !d.is_empty()),
    });
    Question {
        text: asked.question.clone(),
        header: Some(asked.header.clone()).filter(|h| !h.is_empty()),
        options: options.collect(),
        multi_select: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seat's thread runs its commands as the seat and gets Slopty's tools with the seat's
    /// variables as their own; without Slopty's CLI it gets the variables alone.
    #[test]
    fn a_seats_thread_is_loaded_with_its_variables_and_slopty_tools() {
        let env = [("SLOPTY_SESSION", "seat-1"), ("SLOPTY_TASK", "t-1"), ("SLOPTY_SERVER", "s:1")]
            .map(|(n, v)| (n.to_owned(), v.to_owned()));
        let config = seated(&env, Some("/bin/slopty"));
        let set = |var: &str| config[&format!("shell_environment_policy.set.{var}")].clone();
        assert_eq!(set("SLOPTY_SESSION"), "seat-1");
        assert_eq!(set("SLOPTY_TASK"), "t-1");
        assert_eq!(set("SLOPTY_PROJECT"), "", "a variable the seat lacks names nothing");
        assert_eq!(config["mcp_servers.slopty.command"], "/bin/slopty");
        assert_eq!(config["mcp_servers.slopty.args"], serde_json::json!(["mcp"]));
        let vars = serde_json::json!({"SLOPTY_SESSION": "seat-1", "SLOPTY_TASK": "t-1",
            "SLOPTY_SERVER": "s:1"});
        assert_eq!(config["mcp_servers.slopty.env"], vars);
        let bare = seated(&env, None);
        assert!(bare.keys().all(|k| k.starts_with("shell_environment_policy.")), "{bare:?}");
    }

    /// The shell Codex wraps a command in is taken off, its quoting undone; a command it did
    /// not wrap, or one that is not one word to the shell, is as it was.
    #[test]
    fn a_commands_shell_wrapper_is_taken_off() {
        let cases = [
            ("/bin/zsh -lc 'cargo test -p atlas-api refresh'", "cargo test -p atlas-api refresh"),
            ("bash -c \"echo \\\"hi\\\" && ls\"", "echo \"hi\" && ls"),
            ("/bin/sh -c 'it'\\''s here'", "it's here"),
            ("/usr/bin/bash -lc rg", "rg"),
            ("zsh -lc 'line one\nline two'", "line one\nline two"),
            ("cargo test", "cargo test"),
            ("/bin/zsh -lc 'unclosed", "/bin/zsh -lc 'unclosed"),
            ("/bin/zsh -lc 'two' words", "/bin/zsh -lc 'two' words"),
            ("python -c 'print(1)'", "python -c 'print(1)'"),
            ("/bin/zsh -x 'ls'", "/bin/zsh -x 'ls'"),
        ];
        for (command, run) in cases {
            assert_eq!(unwrapped(command), run, "{command}");
        }
        assert_eq!(run_title("cargo test\n--nocapture"), "Run cargo test");
    }
}
