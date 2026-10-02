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

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::detail::{
    Answer, Clip, EditDetail, ExecDetail, ExecStatus, Hunk, McpDetail, Offered, Question,
};
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, Cap, Changed, Choice, Clipped, Compaction, Delivery, Drive,
    Effect, IntentId, Item, ItemBody, ItemId, Liveness, Meters, Notice, PartKey, Patch, Phase,
    Plan, Request, RequestState, Status, Step, ThreadId, ThreadMeta, ToolCall, ToolDetail,
    ToolState, Turn, TurnId, TurnState, Usage, UserMessage, Wait, kind,
};

use super::protocol::{
    self as p, CommandExecutionApprovalDecision, CommandExecutionStatus,
    FileChangeApprovalDecision, PatchApplyStatus, RequestId, ServerNotification, ServerRequest,
    ThreadActiveFlag, ThreadItem, ThreadStatus, TurnStatus, UserInput,
};

/// What a Codex thread can do through Slopty.
pub const CAPS: [&str; 5] =
    [Cap::APPROVALS, Cap::INTERRUPT, Cap::LIVE_TEXT, Cap::LIVE_TUI, Cap::STEER];

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
    let mut hasher = blake3::Hasher::new();
    for part in ["codex thread", native] {
        hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(hasher.finalize().as_bytes().get(..16).unwrap_or(&[0; 16]));
    ThreadId::from_uuid(uuid::Builder::from_custom_bytes(bytes).into_uuid())
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
    /// What each choice answers, by its id: the decision as Codex takes it.
    answers: BTreeMap<String, Value>,
    /// The questions it asks, each with Codex's id for it, which its answer is keyed by.
    questions: Vec<(String, Question)>,
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
    /// The thread's status as Codex last said it, and since when.
    said: Option<(ThreadStatus, WallMs)>,
    /// Each turn as it began, told again once its input is known.
    begun: HashMap<TurnId, Turn>,
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
        let meta = ThreadMeta {
            id,
            agent: AgentId::named(AgentId::CODEX),
            agent_version: thread.cli_version.clone(),
            native: thread.id.clone(),
            cwd: thread.cwd.clone(),
            title: title_of(thread),
            terminal,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
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
                ..Meters::default()
            },
            usage: Usage::default(),
            last_end: None,
            errors: 0,
            said: None,
            begun: HashMap::new(),
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
            ServerNotification::Error(error) if !error.will_retry => {
                self.errors = self.errors.saturating_add(1);
                let item = Item {
                    id: ItemId(format!("error:{}", self.errors)),
                    turn: self.turn(&error.turn_id),
                    at_ms: now,
                    body: ItemBody::Notice(Notice {
                        kind: Notice::API_ERROR.to_owned(),
                        text: Clipped::whole(&error.error.message),
                    }),
                };
                vec![Action::ItemCompleted(item)]
            }
            _ => Vec::new(),
        }
    }

    /// The app-server's request `id` about this thread, heard at `now`.
    pub fn request(&mut self, id: &RequestId, request: &ServerRequest, now: WallMs) -> Vec<Action> {
        let ask = ask_of(id);
        let (item, kind, title, text, options, answers) = match request {
            ServerRequest::ItemCommandExecutionRequestApproval(asked) => {
                let command = asked.command.clone().unwrap_or_default();
                let title = if command.is_empty() {
                    "Run a command?".to_owned()
                } else {
                    format!("Run {command}?")
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
                (None, Request::ELICITATION, title, None, Vec::new(), BTreeMap::new())
            }
        };
        let item = item.map(ItemId);
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
        let open = Open {
            id: id.clone(),
            item: item.clone(),
            title: title.clone(),
            answers,
            questions: questions.clone(),
            answered: None,
        };
        self.open.insert(ask.clone(), open);
        // Codex says the thread waits before it says on what: the wait is worded again now.
        let waiting = self.said.clone().filter(|(status, _)| {
            matches!(status, ThreadStatus::Active { active_flags } if !active_flags.is_empty())
        });
        let request = Request {
            id: ask,
            item,
            kind: kind.to_owned(),
            title,
            text: text.as_deref().map(Clipped::whole),
            options,
            questions: questions.into_iter().map(|(_, q)| q).collect(),
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: None,
        };
        let mut actions = vec![Action::RequestOpened(Box::new(request))];
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
        let result = if open.questions.is_empty() {
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

    /// What goes to the app-server for `text`, sent as intent `intent` with `delivery`: into
    /// the turn under way when it steers and one is, else a turn of its own.
    #[must_use]
    pub fn send(&self, text: &str, delivery: Delivery, intent: IntentId) -> Send {
        let input =
            vec![UserInput::Text { text: text.to_owned(), text_elements: Some(Vec::new()) }];
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
            },
            _ => TurnState::Complete,
        };
        self.last_end = Some(state.clone());
        let mut actions = Vec::new();
        // Whatever the turn still asked is no longer asked.
        let gone: Vec<AskId> = self.open.keys().cloned().collect();
        for ask in gone {
            self.open.remove(&ask);
            actions.push(Action::RequestResolved { id: ask, state: RequestState::Withdrawn });
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
        vec![Action::RequestResolved { id: ask, state: RequestState::Answered { by, choice } }]
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
            ThreadStatus::Active { active_flags } => {
                let what = self.open.values().next().map(|open| open.title.clone());
                if active_flags.contains(&ThreadActiveFlag::WaitingOnApproval) {
                    let text = what.unwrap_or_else(|| "Waits for approval".to_owned());
                    (
                        Phase::NeedsYou,
                        Some(Wait { kind: "permission".to_owned(), text }),
                        Liveness::Live,
                    )
                } else if active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput) {
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
        let item = Item { id, turn, at_ms: at, body };
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
                let exec = ExecDetail {
                    command: Clipped::whole(command),
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
                    title: command.clone(),
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
fn title_of(thread: &p::Thread) -> String {
    thread
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| thread.preview.lines().next().unwrap_or_default().to_owned())
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
