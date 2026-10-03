//! One ACP session, driven by Slopty as its client, mapped onto the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Sans-IO. The worker runs the agent and carries its messages; this turns what the agent says
//! into [`Action`]s, and what a client asks of the thread into what goes to the agent.
//!
//! - **What it can do** is read from the agent: its `initialize` answer says whether its session
//!   can be loaded again, and the session's modes and options say whether its mode and model can be
//!   switched. Every ACP agent's turn can be cancelled, its text streams, and it asks before it
//!   acts.
//! - **Turns.** A prompt is a turn: it opens as the prompt goes, with the person's message as its
//!   input, and ends with the prompt's answer, whose stop reason says how. ACP takes no message
//!   while a turn runs, so one sent then is held on the worker until the turn ends
//!   ([`Session::queue`]).
//! - **Items.** What the agent streams is cut into items where the kind changes (its message, its
//!   thought, a tool call) or its message id does, numbered `item-<n>`; a tool call keeps the
//!   agent's id. A session loaded again replays its history the same way, the person's messages
//!   opening the turns, so a thread read again is the thread that was streamed.
//! - **Approvals, fail closed.** A permission request is a request on its call with exactly the
//!   answers the agent offers. Only one of them goes back. A cancel answers every request still
//!   open as cancelled, as the protocol asks; a request that cannot be read is refused, so the call
//!   does not run.

use std::collections::{BTreeMap, HashMap, VecDeque};

use agent_client_protocol_schema::v1::{self as acp, ContentBlock, SessionUpdate};
use agent_client_protocol_schema::{MaybeUndefined, ProtocolVersion};
use serde_json::Value;
use slopty_core::WallMs;
use slopty_proto::thread::detail::{
    EditDetail, ExecDetail, ExecStatus, FetchDetail, Hunk, Patch, ReadDetail, SearchDetail,
    WriteDetail,
};
use slopty_proto::thread::wire::PastSession;
use slopty_proto::thread::{
    self, Action, Answerer, AskId, Cap, Changed, Clipped, Command, Delivery, Drive, Effect,
    IntentId, Item, ItemBody, ItemId, Liveness, Meters, Mode, Model, Notice, PartKey, Pending,
    PendingState, Phase, Plan, RequestState, Status, Step, ThreadId, ThreadMeta, ThreadState,
    ToolCall, ToolDetail, ToolState, Turn, TurnId, TurnState, UserMessage, Wait, kind,
};

use super::rpc;
use crate::attach::Attached;
use crate::driven::{OUTPUT, PROSE, caps, choice, micro_usd, title_of, tool};

/// What every ACP agent can do through Slopty: its requests are answered here, its turn is
/// cancelled, its text streams, and a message waits on the worker for the turn to end.
pub const CAPS: [&str; 4] = [Cap::APPROVALS, Cap::INTERRUPT, Cap::LIVE_TEXT, Cap::QUEUE];

/// The protocol version spoken.
pub const PROTOCOL: ProtocolVersion = ProtocolVersion::V1;

/// The fact a thread keeps once its agent said it can load a session again (`loadSession`), so a
/// thread read from the log after the agent is gone knows whether it can be taken up again.
pub const LOADABLE_FACT: &str = "acp-load-session";

/// The fact a thread keeps once its agent said it takes pictures in a prompt (`image`), so a
/// message queued before a restart still goes with its pictures as pictures.
pub const PICTURES_FACT: &str = "acp-prompt-image";

/// Whether the thread of `meta` can be taken up again: its agent loads sessions and named this
/// one.
#[must_use]
pub fn resumable(meta: &ThreadMeta) -> bool {
    loads(meta) && !meta.native.is_empty()
}

fn loads(meta: &ThreadMeta) -> bool {
    meta.facts.get(LOADABLE_FACT).is_some_and(|v| v == "true")
}

/// The thread `state` of an agent that ended unheard, as when the worker that ran it stopped:
/// exited, resumable when its session can be loaded again.
#[must_use]
pub fn gone(state: &ThreadState, now: WallMs) -> Vec<Action> {
    crate::driven::cut_short(state, resumable(&state.meta), now)
}

/// The thread a start under intent `intent` makes: the agent names its session only once it is
/// made, so the thread is named by the start.
#[must_use]
pub fn thread_of(intent: IntentId) -> ThreadId {
    ThreadId::derived(&["acp thread", &intent.to_string()])
}

/// What a start's arguments are to take one of the agent's sessions up again rather than begin
/// one: `resume <session>`.
pub const RESUME: &str = "resume";

/// The arguments of a start that takes the agent's session `native` up again.
#[must_use]
pub fn resume_args(native: &str) -> Vec<String> {
    vec![RESUME.to_owned(), native.to_owned()]
}

/// The session a start's `args` take up again, when they are [`resume_args`].
#[must_use]
pub fn resumed(args: &[String]) -> Option<&str> {
    match args {
        [word, native] if word == RESUME && !native.trim().is_empty() => Some(native),
        _ => None,
    }
}

/// `session/list` of the agent's sessions in folder `cwd`, from `cursor` on.
#[must_use]
pub fn list(cwd: &str, cursor: Option<String>) -> acp::ListSessionsRequest {
    acp::ListSessionsRequest::new().cwd(std::path::PathBuf::from(cwd)).cursor(cursor)
}

/// One of the sessions `session/list` listed of `agent`, as a past session: taken up again by
/// [`resume_args`] when the agent `loads` sessions, else by nothing.
#[must_use]
pub fn past(agent: &thread::AgentId, info: &acp::SessionInfo, loads: bool) -> PastSession {
    let updated = info.updated_at.as_deref().and_then(crate::conversation::parse_ms);
    PastSession {
        agent: agent.clone(),
        native: info.session_id.0.to_string(),
        cwd: Some(info.cwd.to_string_lossy().into_owned()),
        prompts: Vec::new(),
        title: info.title.clone().filter(|t| !t.trim().is_empty()),
        updated_ms: updated,
        thread: None,
        resume: if loads { resume_args(&info.session_id.0) } else { Vec::new() },
        facts: BTreeMap::new(),
    }
}

/// `path` as a `file:` URI, every byte but the unreserved ones and `/` escaped.
fn file_uri(path: &str) -> String {
    let mut uri = String::with_capacity(path.len().saturating_add(7));
    uri.push_str("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            for nibble in [byte >> 4, byte & 0x0f] {
                uri.extend(char::from_digit(u32::from(nibble), 16).map(|c| c.to_ascii_uppercase()));
            }
        }
    }
    uri
}

/// Slopty's `initialize`: protocol v1, and no file system or terminal offered, so the agent
/// works with its own tools and asks before it acts.
#[must_use]
pub fn initialize() -> acp::InitializeRequest {
    let client = acp::Implementation::new("slopty", env!("CARGO_PKG_VERSION")).title("Slopty");
    acp::InitializeRequest::new(PROTOCOL)
        .client_capabilities(acp::ClientCapabilities::new())
        .client_info(client)
}

/// Which of the agent's streams an item comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    User,
    Text,
    Thought,
}

/// The item the agent is streaming into.
#[derive(Debug)]
struct Open {
    id: ItemId,
    part: Part,
    text: String,
    message: Option<String>,
}

/// A tool call as the agent last told it.
#[derive(Clone, Debug)]
struct Told {
    kind: acp::ToolKind,
    name: Option<String>,
    title: String,
    status: acp::ToolCallStatus,
    content: Vec<acp::ToolCallContent>,
    locations: Vec<acp::ToolCallLocation>,
    raw_input: Option<Value>,
    raw_output: Option<Value>,
}

/// A permission request open on the thread.
#[derive(Clone, Debug)]
struct Asked {
    id: acp::RequestId,
    options: Vec<acp::PermissionOption>,
    call: String,
    title: String,
}

/// A person's answer, as it goes to the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    /// The request it answers.
    pub id: acp::RequestId,
    /// The answer.
    pub response: acp::RequestPermissionResponse,
    /// What the thread shows of it at once.
    pub actions: Vec<Action>,
}

/// A cancel, as it goes to the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancelled {
    /// `session/cancel`.
    pub notification: acp::CancelNotification,
    /// Every request still open, answered as cancelled.
    pub answers: Vec<(acp::RequestId, acp::RequestPermissionResponse)>,
    /// What the thread shows of it at once.
    pub actions: Vec<Action>,
}

/// A switch of the session's mode or of one of its options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Switch {
    /// `session/set_mode`.
    Mode(acp::SetSessionModeRequest),
    /// `session/set_config_option`.
    Option(acp::SetSessionConfigOptionRequest),
}

/// An ACP session as Slopty shows it.
#[derive(Debug)]
pub struct Session {
    meta: ThreadMeta,
    /// The agent can load a session again (`loadSession`).
    loadable: bool,
    /// The agent takes pictures in a prompt (`promptCapabilities.image`).
    pictures: bool,
    turns: u32,
    turn_open: bool,
    /// The turn under way as it began, told again once its input is known.
    begun: Option<Turn>,
    /// Items numbered so far.
    items: u32,
    open: Option<Open>,
    /// Each call as the agent last told it and as the thread shows it, by the agent's id.
    calls: HashMap<String, (TurnId, Told, ToolCall)>,
    asked: BTreeMap<AskId, Asked>,
    /// Calls the person refused, which end rejected rather than failed.
    denied: Vec<String>,
    queued: VecDeque<Pending>,
    modes: Vec<acp::SessionMode>,
    current_mode: Option<String>,
    options: Vec<acp::SessionConfigOption>,
    meters: Meters,
    /// A prompt is under way.
    running: bool,
    /// A cancel was sent for it.
    cancelling: bool,
    /// The session's history is being replayed (`session/load`).
    replaying: bool,
    last_end: Option<TurnState>,
    phase: (Phase, WallMs),
}

impl Session {
    /// A thread `thread` for ACP agent `agent` (`acp:<name>`) in `cwd`, with the actions that
    /// begin it. Its session is named once the agent makes it ([`Session::opened`]).
    #[must_use]
    pub fn new(
        agent: thread::AgentId,
        thread: ThreadId,
        cwd: &str,
        now: WallMs,
    ) -> (Self, Vec<Action>) {
        let meta = ThreadMeta {
            modes: Vec::new(),
            id: thread,
            agent,
            agent_version: String::new(),
            native: String::new(),
            cwd: cwd.to_owned(),
            title: String::new(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::DRIVEN),
            caps: caps(&CAPS),
            models: Vec::new(),
            facts: BTreeMap::new(),
            created_ms: now,
        };
        let session = Self::with(meta, now);
        let actions = vec![Action::Meta(Box::new(session.meta.clone())), session.status_now()];
        (session, actions)
    }

    /// The thread `state` as it stands, to go on from: what the agent says next is numbered
    /// after what it holds. For a thread read again from nothing, give it the state the host
    /// was reset to.
    #[must_use]
    pub fn of(state: &ThreadState, now: WallMs) -> Self {
        let mut session = Self::with(state.meta.clone(), now);
        session.turns = state.last_turn().map_or(0, |t| t.id.0);
        session.items = state
            .items
            .iter()
            .filter_map(|i| i.id.0.strip_prefix("item-").and_then(|n| n.parse().ok()))
            .max()
            .unwrap_or(0);
        session.queued = state.pending.iter().cloned().collect();
        session.meters = state.meters.clone();
        session.last_end = state.last_turn().map(|t| t.state.clone());
        session.phase = (state.status.phase, state.status.since_ms);
        session
    }

    fn with(meta: ThreadMeta, now: WallMs) -> Self {
        Self {
            loadable: loads(&meta),
            pictures: meta.facts.get(PICTURES_FACT).is_some_and(|v| v == "true"),
            meta,
            turns: 0,
            turn_open: false,
            begun: None,
            items: 0,
            open: None,
            calls: HashMap::new(),
            asked: BTreeMap::new(),
            denied: Vec::new(),
            queued: VecDeque::new(),
            modes: Vec::new(),
            current_mode: None,
            options: Vec::new(),
            meters: Meters::default(),
            running: false,
            cancelling: false,
            replaying: false,
            last_end: None,
            phase: (Phase::Idle, now),
        }
    }

    /// What the thread is.
    #[must_use]
    pub const fn meta(&self) -> &ThreadMeta {
        &self.meta
    }

    /// Whether a prompt is under way.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    /// Whether the session can be taken up again by loading it.
    #[must_use]
    pub const fn resumable(&self) -> bool {
        self.loadable && !self.meta.native.is_empty()
    }

    /// The agent's answer to [`initialize`].
    ///
    /// # Errors
    ///
    /// When the agent speaks another version of the protocol, in words.
    pub fn initialized(
        &mut self,
        response: &acp::InitializeResponse,
    ) -> Result<Vec<Action>, String> {
        if response.protocol_version != PROTOCOL {
            return Err(format!(
                "the agent speaks ACP version {}, and Slopty speaks {PROTOCOL}",
                response.protocol_version
            ));
        }
        let can = &response.agent_capabilities;
        self.loadable = can.load_session;
        self.pictures = can.prompt_capabilities.image;
        self.meta.facts.insert(LOADABLE_FACT.to_owned(), self.loadable.to_string());
        self.meta.facts.insert(PICTURES_FACT.to_owned(), self.pictures.to_string());
        let fork = Cap::named(Cap::FORK);
        self.meta.caps.retain(|c| *c != fork);
        if can.session_capabilities.fork.is_some() {
            self.meta.caps.push(fork);
            self.meta.caps.sort();
        }
        if let Some(info) = &response.agent_info {
            self.meta.agent_version.clone_from(&info.version);
        }
        Ok(vec![Action::Meta(Box::new(self.meta.clone()))])
    }

    /// `session/new` for the thread's folder.
    #[must_use]
    pub fn session_new(&self) -> acp::NewSessionRequest {
        acp::NewSessionRequest::new(self.meta.cwd.clone())
    }

    /// `session/fork` of the agent's session `from` into this thread's folder: the fork is this
    /// thread's session once the agent answers ([`Session::opened`]).
    #[must_use]
    pub fn session_fork(&self, from: &str) -> acp::ForkSessionRequest {
        acp::ForkSessionRequest::new(from.to_owned(), self.meta.cwd.clone())
    }

    /// This thread branched off another as `fork` says: it says so from its start.
    pub fn forked(&mut self, fork: thread::Fork) {
        self.meta.forked_from = Some(fork);
        ThreadMeta::FORK.clone_into(&mut self.meta.origin);
    }

    /// This thread takes up the agent's session `native`, made before it: it is loaded once the
    /// agent runs ([`Session::session_load`]).
    pub fn resuming(&mut self, native: &str) {
        native.clone_into(&mut self.meta.native);
    }

    /// `session/load` for the thread's session, when the agent can load it. The agent replays
    /// its history before it answers, which rebuilds the thread from nothing.
    pub fn session_load(&mut self) -> Option<acp::LoadSessionRequest> {
        if !self.resumable() {
            return None;
        }
        self.replaying = true;
        Some(acp::LoadSessionRequest::new(self.meta.native.clone(), self.meta.cwd.clone()))
    }

    /// The session is made or loaded: named `session` (when the agent named it), with its
    /// `modes` and `options`. A history replayed before it ends here.
    pub fn opened(
        &mut self,
        session: Option<&acp::SessionId>,
        modes: Option<&acp::SessionModeState>,
        options: Option<&[acp::SessionConfigOption]>,
        now: WallMs,
    ) -> Vec<Action> {
        let mut actions = self.close(now);
        if std::mem::take(&mut self.replaying) {
            actions.extend(self.finish_calls(now));
            actions.extend(self.end_turn(now));
        }
        if let Some(session) = session {
            self.meta.native = session.0.to_string();
        }
        if let Some(modes) = modes {
            self.modes.clone_from(&modes.available_modes);
            self.current_mode = Some(modes.current_mode_id.0.to_string());
        }
        if let Some(options) = options {
            self.options = options.to_vec();
        }
        actions.extend(self.settings_changed());
        actions.push(self.status(now));
        actions
    }

    /// A `session/update` the agent sent with `params`.
    pub fn update(&mut self, params: &Value, now: WallMs) -> Vec<Action> {
        let Ok(notification) = serde_json::from_value::<acp::SessionNotification>(params.clone())
        else {
            let update = params.get("update").cloned().unwrap_or(Value::Null);
            let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("update");
            return self.extra(kind, &update, now);
        };
        match &notification.update {
            SessionUpdate::UserMessageChunk(chunk) => self.chunk(Part::User, chunk, now),
            SessionUpdate::AgentMessageChunk(chunk) => self.chunk(Part::Text, chunk, now),
            SessionUpdate::AgentThoughtChunk(chunk) => self.chunk(Part::Thought, chunk, now),
            SessionUpdate::ToolCall(call) => {
                let mut actions = self.close(now);
                actions.extend(self.tool_call(call, now));
                actions.extend(self.tally());
                actions
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let mut actions = self.close(now);
                actions.extend(self.tool_update(update, now));
                actions.extend(self.tally());
                actions
            }
            SessionUpdate::Plan(plan) => {
                let steps: Vec<Step> = plan
                    .entries
                    .iter()
                    .map(|entry| Step {
                        id: None,
                        text: entry.content.clone(),
                        status: plan_status(&entry.status).to_owned(),
                    })
                    .collect();
                let plan = (!steps.is_empty()).then_some(Plan { text: None, steps });
                vec![Action::PlanSet(plan)]
            }
            SessionUpdate::AvailableCommandsUpdate(update) => {
                let commands = update
                    .available_commands
                    .iter()
                    .map(|c| Command {
                        name: c.name.clone(),
                        description: c.description.clone(),
                        argument_hint: c.input.as_ref().and_then(|input| match input {
                            acp::AvailableCommandInput::Unstructured(i) => Some(i.hint.clone()),
                            _ => None,
                        }),
                        source: "agent".to_owned(),
                    })
                    .collect();
                vec![Action::CommandsSet(commands)]
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                self.current_mode = Some(update.current_mode_id.0.to_string());
                self.settings_changed()
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.options.clone_from(&update.config_options);
                self.settings_changed()
            }
            SessionUpdate::SessionInfoUpdate(update) => match &update.title {
                MaybeUndefined::Value(title) if !title.trim().is_empty() => {
                    self.meta.title.clone_from(title);
                    vec![Action::Meta(Box::new(self.meta.clone()))]
                }
                _ => Vec::new(),
            },
            SessionUpdate::UsageUpdate(usage) => {
                let mut meters = self.meters.clone();
                meters.context_tokens = Some(usage.used);
                meters.context_window = Some(usage.size);
                if let Some(cost) = usage.cost.as_ref().filter(|c| c.currency == "USD") {
                    meters.cost_micro_usd = Some(micro_usd(cost.amount));
                }
                self.meters_now(meters)
            }
            other => {
                let update = serde_json::to_value(other).unwrap_or(Value::Null);
                let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("update");
                let kind = kind.to_owned();
                self.extra(&kind, &update, now)
            }
        }
    }

    /// What goes to the agent for `text` and the files `attached`, sent as intent `intent`, and
    /// the turn it opens. A picture goes as an image block where the agent takes pictures, else
    /// as a link to its file, as any other file goes: a resource link every agent takes.
    pub fn prompt(
        &mut self,
        text: &str,
        attached: &[Attached],
        intent: IntentId,
        now: WallMs,
    ) -> (acp::PromptRequest, Vec<Action>) {
        let mut actions = self.close(now);
        actions.extend(self.open_turn(now));
        let id = self.next_item();
        actions.extend(self.user_message(&id, text, Some(intent), now));
        self.running = true;
        self.cancelling = false;
        actions.push(self.status(now));
        let words =
            (!text.trim().is_empty()).then(|| ContentBlock::Text(acp::TextContent::new(text)));
        let files = attached.iter().map(|file| self.block(file));
        let prompt = words.into_iter().chain(files).collect();
        (acp::PromptRequest::new(self.meta.native.clone(), prompt), actions)
    }

    /// The block a file sent with a message is.
    fn block(&self, file: &Attached) -> ContentBlock {
        let uri = file_uri(file.path());
        match (file, file.base64()) {
            (Attached::Picture { media_type, .. }, Some(data)) if self.pictures => {
                ContentBlock::Image(acp::ImageContent::new(data, *media_type).uri(uri))
            }
            _ => ContentBlock::ResourceLink(acp::ResourceLink::new(file.name(), uri)),
        }
    }

    /// The prompt's answer: how the turn ended, or why the agent refused it.
    pub fn prompted(&mut self, outcome: Result<&Value, &acp::Error>, now: WallMs) -> Vec<Action> {
        let mut actions = self.close(now);
        let end = match outcome {
            Ok(result) => match serde_json::from_value::<acp::PromptResponse>(result.clone()) {
                Ok(response) => {
                    // The turn's tokens, when the agent counts them with its answer.
                    if let (Some(usage), Some(begun)) = (&response.usage, self.begun.as_mut()) {
                        begun.usage = usage_of(usage);
                    }
                    match response.stop_reason {
                        acp::StopReason::Cancelled => TurnState::Interrupted,
                        acp::StopReason::Refusal => {
                            actions.extend(self.notice(
                                Notice::INFO,
                                "The agent refused to go on.",
                                now,
                            ));
                            TurnState::Failed { error: "refused".to_owned(), until_ms: None }
                        }
                        acp::StopReason::MaxTokens => {
                            actions.extend(self.notice(
                                Notice::INFO,
                                "The agent stopped at its token limit.",
                                now,
                            ));
                            TurnState::Complete
                        }
                        acp::StopReason::MaxTurnRequests => {
                            actions.extend(self.notice(
                                Notice::INFO,
                                "The agent stopped at its limit of model requests for a turn.",
                                now,
                            ));
                            TurnState::Complete
                        }
                        _ => TurnState::Complete,
                    }
                }
                Err(e) => {
                    let why = format!("The agent's answer did not read: {e}");
                    actions.extend(self.notice(Notice::INFO, &why, now));
                    TurnState::Failed { error: why, until_ms: None }
                }
            },
            Err(_) if self.cancelling => TurnState::Interrupted,
            Err(error) => {
                let why = said(error);
                actions.extend(self.notice(Notice::API_ERROR, &why, now));
                TurnState::Failed { error: why, until_ms: None }
            }
        };
        self.last_end = Some(end);
        actions.extend(self.withdraw_all());
        actions.extend(self.finish_calls(now));
        self.running = false;
        actions.extend(self.end_turn(now));
        self.cancelling = false;
        actions.push(self.status(now));
        actions
    }

    /// The agent's permission request `id`, with `params`: a request on its call with the
    /// answers it offers.
    ///
    /// # Errors
    ///
    /// When it cannot be read or offers no answer: the agent is refused, so the call does not
    /// run.
    pub fn permission(
        &mut self,
        id: &acp::RequestId,
        params: &Value,
        now: WallMs,
    ) -> Result<Vec<Action>, acp::Error> {
        let request: acp::RequestPermissionRequest = serde_json::from_value(params.clone())
            .map_err(|e| acp::Error::invalid_params().data(Value::String(e.to_string())))?;
        if request.options.is_empty() {
            return Err(acp::Error::invalid_params().data(Value::from("no answer is offered")));
        }
        let call = request.tool_call.tool_call_id.0.to_string();
        let mut actions = self.close(now);
        actions.extend(self.tool_update(&request.tool_call, now));
        let title = self
            .calls
            .get(&call)
            .map_or_else(|| "Go ahead?".to_owned(), |(_, _, shown)| format!("{}?", shown.title));
        let ask = AskId(rpc::id_text(id));
        let options = request
            .options
            .iter()
            .map(|o| {
                let (effect, scope) = match o.kind {
                    acp::PermissionOptionKind::AllowOnce => (Effect::Allow, None),
                    acp::PermissionOptionKind::AllowAlways => (Effect::Allow, Some("always")),
                    acp::PermissionOptionKind::RejectOnce => (Effect::Deny, None),
                    acp::PermissionOptionKind::RejectAlways => (Effect::Deny, Some("always")),
                    _ => (Effect::Answer, None),
                };
                let mut offered = choice(&o.option_id.0, &o.name, effect, false);
                offered.scope = scope.map(str::to_owned);
                offered
            })
            .collect();
        let asked = Asked {
            id: id.clone(),
            options: request.options,
            call: call.clone(),
            title: title.clone(),
        };
        self.asked.insert(ask.clone(), asked);
        let card = thread::Request {
            editable: Vec::new(),
            id: ask.clone(),
            item: Some(ItemId(call.clone())),
            kind: thread::Request::APPROVAL.to_owned(),
            title,
            text: None,
            options,
            questions: Vec::new(),
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: None,
        };
        actions.push(Action::RequestOpened(Box::new(card)));
        actions.extend(self.call_state(&call, ToolState::Pending { ask }, now));
        actions.push(self.status(now));
        Ok(actions)
    }

    /// Whether request `ask` is open and offers `choice`.
    #[must_use]
    pub fn takes(&self, ask: &AskId, choice: &str) -> bool {
        self.asked
            .get(ask)
            .is_some_and(|asked| asked.options.iter().any(|o| *o.option_id.0 == *choice))
    }

    /// The answer `choice` to request `ask`, given by `by` at `now`. `None` when no such
    /// request is open or it offers no such answer.
    pub fn answer(
        &mut self,
        ask: &AskId,
        choice: &str,
        by: Answerer,
        now: WallMs,
    ) -> Option<Answered> {
        if !self.takes(ask, choice) {
            return None;
        }
        let asked = self.asked.remove(ask)?;
        let option = asked.options.iter().find(|o| *o.option_id.0 == *choice)?;
        let allows = matches!(
            option.kind,
            acp::PermissionOptionKind::AllowOnce | acp::PermissionOptionKind::AllowAlways
        );
        let selected = acp::SelectedPermissionOutcome::new(option.option_id.clone());
        let response =
            acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Selected(selected));
        let mut actions = vec![Action::RequestResolved {
            id: ask.clone(),
            state: RequestState::Answered { by, choice: choice.to_owned() },
        }];
        if allows {
            actions.extend(self.call_state(&asked.call, ToolState::Running, now));
        } else {
            self.denied.push(asked.call);
        }
        actions.push(self.status(now));
        Some(Answered { id: asked.id, response, actions })
    }

    /// What cancels the turn under way, when one is: `session/cancel`, and every request still
    /// open answered as cancelled.
    pub fn cancel(&mut self, now: WallMs) -> Option<Cancelled> {
        if !self.running {
            return None;
        }
        self.cancelling = true;
        let answers = self
            .asked
            .values()
            .map(|asked| {
                let response =
                    acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled);
                (asked.id.clone(), response)
            })
            .collect();
        let mut actions = self.withdraw_all();
        actions.push(self.status(now));
        let notification = acp::CancelNotification::new(self.meta.native.clone());
        Some(Cancelled { notification, answers, actions })
    }

    /// Hold `text` and the files at `attachments`, sent as intent `intent`, until the turn ends.
    pub fn queue(&mut self, intent: IntentId, text: &str, attachments: Vec<String>) -> Vec<Action> {
        self.queued.push_back(Pending {
            intent,
            text: text.to_owned(),
            attachments,
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        });
        vec![self.pending_now()]
    }

    /// Take back the message held for `intent`; `None` when none is.
    pub fn withdraw(&mut self, intent: IntentId) -> Option<Vec<Action>> {
        let at = self.queued.iter().position(|p| p.intent == intent)?;
        self.queued.remove(at);
        Some(vec![self.pending_now()])
    }

    /// Make the message held for `intent` say `text`; `None` when none is.
    pub fn edit(&mut self, intent: IntentId, text: &str) -> Option<Vec<Action>> {
        let held = self.queued.iter_mut().find(|p| p.intent == intent)?;
        text.clone_into(&mut held.text);
        Some(vec![self.pending_now()])
    }

    /// Move the message held for `intent` to just before the one held for `before`, or to the
    /// end; `None` when either is not held.
    pub fn reorder(&mut self, intent: IntentId, before: Option<IntentId>) -> Option<Vec<Action>> {
        let queued = self.queued.make_contiguous();
        Pending::reorder(queued, |p| p.intent, intent, before).then(|| vec![self.pending_now()])
    }

    /// The next message held, taken off the queue, once no turn is under way: its intent, its
    /// words and its files.
    pub fn next_queued(&mut self) -> Option<(Pending, Vec<Action>)> {
        if self.running {
            return None;
        }
        let next = self.queued.pop_front()?;
        Some((next, vec![self.pending_now()]))
    }

    /// What switches the session to mode `mode` (an id it offers), when it can be.
    #[must_use]
    pub fn set_mode(&self, mode: &str) -> Option<Switch> {
        if let Some(option) = self.option_of(&acp::SessionConfigOptionCategory::Mode) {
            return select_offers(option, mode).then(|| self.set_option(option, mode));
        }
        self.modes.iter().any(|m| *m.id.0 == *mode).then(|| {
            Switch::Mode(acp::SetSessionModeRequest::new(self.meta.native.clone(), mode.to_owned()))
        })
    }

    /// What switches the session to model `model` (an id it offers), when it can be.
    #[must_use]
    pub fn set_model(&self, model: &str) -> Option<Switch> {
        let option = self.option_of(&acp::SessionConfigOptionCategory::Model)?;
        select_offers(option, model).then(|| self.set_option(option, model))
    }

    /// The session's mode is `mode` now, as the agent agreed.
    pub fn mode_set(&mut self, mode: &str) -> Vec<Action> {
        self.current_mode = Some(mode.to_owned());
        self.settings_changed()
    }

    /// The session's options are `options` now, as the agent answered a switch.
    pub fn options_set(&mut self, options: &[acp::SessionConfigOption]) -> Vec<Action> {
        self.options = options.to_vec();
        self.settings_changed()
    }

    /// The agent is gone, at `now`, for the reason in `why` when it failed: what it was doing
    /// is cut short, and the thread can be taken up again when its session can be loaded.
    pub fn exited(&mut self, why: Option<&str>, now: WallMs) -> Vec<Action> {
        let mut actions = self.close(now);
        actions.extend(self.withdraw_all());
        let dangling: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, (_, _, call))| !call.state.is_final())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dangling {
            actions.extend(self.call_state(&id, ToolState::Cancelled, now));
        }
        if let Some(why) = why {
            actions.extend(self.notice(Notice::INFO, why, now));
        }
        if self.turn_open {
            self.last_end = Some(match why {
                Some(why) => TurnState::Failed { error: why.to_owned(), until_ms: None },
                None => TurnState::Interrupted,
            });
        }
        self.running = false;
        self.replaying = false;
        actions.extend(self.end_turn(now));
        self.phase = (self.phase_now(), now);
        actions.push(Action::Status(Status {
            phase: self.phase.0,
            wait: None,
            liveness: Liveness::Exited { resumable: self.resumable() },
            since_ms: now,
        }));
        actions
    }

    fn next_item(&mut self) -> ItemId {
        self.items = self.items.saturating_add(1);
        ItemId(format!("item-{}", self.items))
    }

    const fn turn(&self) -> TurnId {
        TurnId(self.turns)
    }

    /// The turn under way told again when the lines its calls' diffs change moved.
    fn tally(&mut self) -> Option<Action> {
        let turn = self.turn();
        let changed = self
            .calls
            .values()
            .filter(|(t, ..)| *t == turn)
            .filter_map(|(_, _, call)| match &call.detail {
                Some(ToolDetail::Edit(edit)) => Some(&edit.patch),
                Some(ToolDetail::Write(write)) => Some(&write.patch),
                _ => None,
            })
            .fold(Changed::default(), |sum, patch| Changed {
                added: sum.added.saturating_add(patch.added),
                removed: sum.removed.saturating_add(patch.removed),
            });
        let begun = self.begun.as_mut().filter(|b| b.changed != changed)?;
        begun.changed = changed;
        Some(Action::TurnStarted(begun.clone()))
    }

    fn open_turn(&mut self, now: WallMs) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.turn_open {
            actions.extend(self.finish_calls(now));
            actions.extend(self.end_turn(now));
        }
        self.turns = self.turns.saturating_add(1);
        self.turn_open = true;
        self.last_end = Some(TurnState::Active);
        let begun = Turn {
            id: self.turn(),
            input: None,
            state: TurnState::Active,
            started_ms: now,
            ended_ms: None,
            usage: thread::Usage::default(),
            models: self.meters.model.iter().cloned().collect(),
            changed: Changed::default(),
            before: None,
            after: None,
        };
        self.begun = Some(begun.clone());
        actions.push(Action::TurnStarted(begun));
        actions
    }

    fn end_turn(&mut self, now: WallMs) -> Vec<Action> {
        if !self.turn_open {
            return Vec::new();
        }
        self.turn_open = false;
        let state = match self.last_end.take() {
            Some(TurnState::Active) | None => TurnState::Complete,
            Some(state) => state,
        };
        self.last_end = Some(state.clone());
        let usage = self.begun.take().map(|t| t.usage).unwrap_or_default();
        vec![Action::TurnEnded { turn: self.turn(), state, usage, ended_ms: now }]
    }

    /// The person's message `text` as item `id`, the input of the turn under way.
    fn user_message(
        &mut self,
        id: &ItemId,
        text: &str,
        intent: Option<IntentId>,
        now: WallMs,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(begun) = self.begun.as_mut()
            && begun.input.is_none()
        {
            begun.input = Some(id.clone());
            actions.push(Action::TurnStarted(begun.clone()));
        }
        if self.meta.title.is_empty() && !text.trim().is_empty() {
            self.meta.title = title_of(text);
            actions.push(Action::Meta(Box::new(self.meta.clone())));
        }
        let body = ItemBody::User(UserMessage {
            text: Clipped::head(text, PROSE, None),
            images: Vec::new(),
            command: None,
            intent,
        });
        actions.push(Action::ItemCompleted(Item {
            id: id.clone(),
            turn: self.turn(),
            at_ms: now,
            body,
        }));
        actions
    }

    /// A chunk of `part`: it goes on the item streaming when it is of the same message, and
    /// begins the next one when it is not.
    fn chunk(&mut self, part: Part, chunk: &acp::ContentChunk, now: WallMs) -> Vec<Action> {
        // Live, the person's message is the prompt's own; only a history replays it.
        if part == Part::User && !self.replaying {
            return Vec::new();
        }
        let text = match &chunk.content {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::ResourceLink(link) => {
                format!("[{}]({})", link.title.as_deref().unwrap_or(&link.name), link.uri)
            }
            other => {
                let json = serde_json::to_value(other).unwrap_or(Value::Null);
                let kind = json.get("type").and_then(Value::as_str).unwrap_or("content").to_owned();
                let mut actions = self.close(now);
                actions.extend(self.extra(&kind, &json, now));
                return actions;
            }
        };
        let message = chunk.message_id.as_ref().map(|m| m.0.to_string());
        let mut actions = Vec::new();
        let goes_on = self.open.as_ref().is_some_and(|open| {
            open.part == part
                && (open.message.is_none() || message.is_none() || open.message == message)
        });
        if !goes_on {
            actions.extend(self.close(now));
            if part == Part::User {
                actions.extend(self.open_turn(now));
            }
            let id = self.next_item();
            if part != Part::User && !self.replaying {
                let body = body_of(part, "");
                actions.push(Action::ItemStarted(Item {
                    id: id.clone(),
                    turn: self.turn(),
                    at_ms: now,
                    body,
                }));
            }
            self.open = Some(Open { id, part, text: String::new(), message });
        }
        if let Some(open) = self.open.as_mut() {
            open.text.push_str(&text);
            if part != Part::User && !self.replaying && !text.is_empty() {
                actions.push(Action::Append { item: open.id.clone(), part: PartKey::Body, text });
            }
        }
        actions
    }

    /// The item streaming, made whole.
    fn close(&mut self, now: WallMs) -> Vec<Action> {
        let Some(open) = self.open.take() else { return Vec::new() };
        if open.part == Part::User {
            return self.user_message(&open.id, &open.text, None, now);
        }
        let body = body_of(open.part, &open.text);
        vec![Action::ItemCompleted(Item { id: open.id, turn: self.turn(), at_ms: now, body })]
    }

    fn tool_call(&mut self, call: &acp::ToolCall, now: WallMs) -> Vec<Action> {
        let raw = Told {
            kind: call.kind,
            name: call.name.clone(),
            title: call.title.clone(),
            status: call.status,
            content: call.content.clone(),
            locations: call.locations.clone(),
            raw_input: call.raw_input.clone(),
            raw_output: call.raw_output.clone(),
        };
        self.told(&call.tool_call_id.0, raw, now)
    }

    fn tool_update(&mut self, update: &acp::ToolCallUpdate, now: WallMs) -> Vec<Action> {
        let id = update.tool_call_id.0.to_string();
        let fields = &update.fields;
        let mut raw = self.calls.get(&id).map_or_else(
            || Told {
                kind: acp::ToolKind::Other,
                name: None,
                title: String::new(),
                status: acp::ToolCallStatus::Pending,
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                raw_output: None,
            },
            |(_, raw, _)| raw.clone(),
        );
        if let Some(kind) = fields.kind {
            raw.kind = kind;
        }
        if let Some(status) = fields.status {
            raw.status = status;
        }
        if let Some(title) = &fields.title {
            raw.title.clone_from(title);
        }
        if let Some(name) = &fields.name {
            raw.name = Some(name.clone());
        }
        if let Some(content) = &fields.content {
            // A diff stays once told: OpenCode shows a write's diff only while it asks, and
            // ends the call with its output in place of it.
            let kept: Vec<acp::ToolCallContent> = if content.iter().any(is_diff) {
                Vec::new()
            } else {
                raw.content.iter().filter(|c| is_diff(c)).cloned().collect()
            };
            raw.content.clone_from(content);
            raw.content.extend(kept);
        }
        if let Some(locations) = &fields.locations {
            raw.locations.clone_from(locations);
        }
        if let Some(input) = &fields.raw_input {
            raw.raw_input = Some(input.clone());
        }
        if let Some(output) = &fields.raw_output {
            raw.raw_output = Some(output.clone());
        }
        self.told(&id, raw, now)
    }

    /// Call `id` as the agent tells it now.
    fn told(&mut self, id: &str, raw: Told, now: WallMs) -> Vec<Action> {
        let turn = self.turn();
        let wanted = self.state_of(id, raw.status);
        let mut call = shown(&raw);
        let started =
            self.calls.get(id).map(|(turn, _, old)| (*turn, old.state.clone(), old.ended_ms));
        let started_at = started.as_ref().map(|(turn, ..)| *turn);
        if let Some((_, old, ended_ms)) = started {
            // A call that waits on the person stays so until the person answers.
            let waits = matches!(old, ToolState::Pending { .. })
                && self.asked.values().any(|a| a.call == id)
                && !wanted.is_final();
            if old.is_final() {
                call.ended_ms = ended_ms;
                call.state = old;
            } else if old.may_become(&wanted) && !waits {
                call.state = wanted;
                finish(&mut call, now);
            } else {
                call.state = old;
            }
        } else {
            call.state = wanted;
            finish(&mut call, now);
        }
        let turn = started_at.unwrap_or(turn);
        let item = Item { id: ItemId(id.to_owned()), turn, at_ms: now, body: tool(call.clone()) };
        let ended = call.state.is_final();
        let action = if ended {
            Action::ItemCompleted(item)
        } else if started_at.is_some() {
            Action::ItemUpdated(item)
        } else {
            Action::ItemStarted(item)
        };
        self.calls.insert(id.to_owned(), (turn, raw, call));
        let mut actions = vec![action];
        if ended {
            // An ask about a call that ended is no longer asked.
            actions.extend(self.withdraw_where(|a| a.call == id));
        }
        actions.push(self.status(now));
        actions
    }

    /// The state the agent's `status` means for call `id`.
    fn state_of(&self, id: &str, status: acp::ToolCallStatus) -> ToolState {
        match status {
            acp::ToolCallStatus::InProgress => ToolState::Running,
            acp::ToolCallStatus::Completed => ToolState::Completed,
            acp::ToolCallStatus::Failed if self.denied.iter().any(|d| d == id) => {
                ToolState::Rejected
            }
            acp::ToolCallStatus::Failed if self.cancelling => ToolState::Cancelled,
            acp::ToolCallStatus::Failed => ToolState::Failed,
            _ => ToolState::Streaming,
        }
    }

    fn call_state(&mut self, id: &str, state: ToolState, now: WallMs) -> Vec<Action> {
        let Some((turn, _, call)) = self.calls.get_mut(id) else { return Vec::new() };
        if !call.state.may_become(&state) {
            return Vec::new();
        }
        call.state = state;
        finish(call, now);
        let item =
            Item { id: ItemId(id.to_owned()), turn: *turn, at_ms: now, body: tool(call.clone()) };
        vec![if call.state.is_final() {
            Action::ItemCompleted(item)
        } else {
            Action::ItemUpdated(item)
        }]
    }

    /// Calls left unfinished as the turn ends, ended as it did: cancelled with it, failed with
    /// it, else done, since the agent ended its turn on them.
    fn finish_calls(&mut self, now: WallMs) -> Vec<Action> {
        let state = match &self.last_end {
            Some(TurnState::Interrupted) => ToolState::Cancelled,
            Some(TurnState::Failed { .. }) => ToolState::Failed,
            _ if self.cancelling => ToolState::Cancelled,
            _ => ToolState::Completed,
        };
        let turn = self.turn();
        let dangling: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, (at, _, call))| *at == turn && !call.state.is_final())
            .map(|(id, _)| id.clone())
            .collect();
        dangling.iter().flat_map(|id| self.call_state(id, state.clone(), now)).collect()
    }

    fn withdraw_all(&mut self) -> Vec<Action> {
        self.withdraw_where(|_| true)
    }

    fn withdraw_where(&mut self, which: impl Fn(&Asked) -> bool) -> Vec<Action> {
        let gone: Vec<AskId> =
            self.asked.iter().filter(|(_, a)| which(a)).map(|(id, _)| id.clone()).collect();
        gone.into_iter()
            .map(|id| {
                self.asked.remove(&id);
                Action::RequestResolved { id, state: RequestState::Withdrawn }
            })
            .collect()
    }

    fn notice(&mut self, kind: &str, text: &str, now: WallMs) -> Vec<Action> {
        let id = self.next_item();
        let body = ItemBody::Notice(Notice::new(kind, Clipped::whole(text)));
        vec![Action::ItemCompleted(Item { id, turn: self.turn(), at_ms: now, body })]
    }

    /// Something the agent said that this does not map, kept as it came.
    fn extra(&mut self, kind: &str, json: &Value, now: WallMs) -> Vec<Action> {
        let id = self.next_item();
        let body = ItemBody::Extra {
            kind: kind.to_owned(),
            json: Clipped::head(&json.to_string(), OUTPUT, None),
        };
        vec![Action::ItemCompleted(Item { id, turn: self.turn(), at_ms: now, body })]
    }

    fn option_of(
        &self,
        category: &acp::SessionConfigOptionCategory,
    ) -> Option<&acp::SessionConfigOption> {
        self.options.iter().find(|o| {
            o.category.as_ref() == Some(category)
                && matches!(o.kind, acp::SessionConfigKind::Select(_))
        })
    }

    fn set_option(&self, option: &acp::SessionConfigOption, value: &str) -> Switch {
        Switch::Option(acp::SetSessionConfigOptionRequest::new(
            self.meta.native.clone(),
            option.id.clone(),
            acp::SessionConfigOptionValue::ValueId { value: value.to_owned().into() },
        ))
    }

    /// The meta and meters as the session's modes and options leave them.
    fn settings_changed(&mut self) -> Vec<Action> {
        let mut meta = self.meta.clone();
        let mut meters = self.meters.clone();
        let model = self.option_of(&acp::SessionConfigOptionCategory::Model).cloned();
        let mode = self.option_of(&acp::SessionConfigOptionCategory::Mode).cloned();
        meters.effort =
            self.option_of(&acp::SessionConfigOptionCategory::ThoughtLevel).and_then(|option| {
                let current = select_current(option);
                select_choices(option)
                    .into_iter()
                    .find(|(id, _)| Some(id) == current.as_ref())
                    .map(|(_, label)| label)
            });
        meta.models = model
            .as_ref()
            .map(|o| select_choices(o).into_iter().map(|(id, label)| Model { id, label }).collect())
            .unwrap_or_default();
        if let Some(model) = &model {
            let current = select_current(model);
            meters.model = select_choices(model)
                .into_iter()
                .find(|(id, _)| Some(id) == current.as_ref())
                .map(|(_, label)| label);
            meters.model_id = current;
        }
        meta.modes = match &mode {
            Some(option) => select_choices(option)
                .into_iter()
                .map(|(id, label)| Mode { id, label, description: None })
                .collect(),
            None => self
                .modes
                .iter()
                .map(|m| Mode {
                    id: m.id.0.to_string(),
                    label: m.name.clone(),
                    description: m.description.clone(),
                })
                .collect(),
        };
        meters.mode = match &mode {
            Some(option) => {
                let current = select_current(option);
                select_choices(option)
                    .into_iter()
                    .find(|(id, _)| Some(id) == current.as_ref())
                    .map(|(_, label)| label)
            }
            None => self.current_mode.as_ref().map(|current| {
                self.modes
                    .iter()
                    .find(|m| *m.id.0 == **current)
                    .map_or_else(|| current.clone(), |m| m.name.clone())
            }),
        };
        let mut names: Vec<&str> = CAPS.to_vec();
        // Whether it forks is said once, as it starts.
        if self.meta.can(Cap::FORK) {
            names.push(Cap::FORK);
        }
        if mode.is_some() || !self.modes.is_empty() {
            names.push(Cap::SET_MODE);
        }
        if model.is_some() {
            names.push(Cap::SET_MODEL);
        }
        meta.caps = caps(&names);
        let mut actions = Vec::new();
        if meta != self.meta {
            self.meta = meta;
            actions.push(Action::Meta(Box::new(self.meta.clone())));
        }
        actions.extend(self.meters_now(meters));
        actions
    }

    fn meters_now(&mut self, meters: Meters) -> Vec<Action> {
        if meters == self.meters {
            return Vec::new();
        }
        self.meters = meters;
        vec![Action::MetersSet(self.meters.clone())]
    }

    fn pending_now(&self) -> Action {
        Action::PendingSet(self.queued.iter().cloned().collect())
    }

    /// The status as it stands, since the time its phase last moved.
    fn status(&mut self, now: WallMs) -> Action {
        let phase = self.phase_now();
        if phase != self.phase.0 {
            self.phase = (phase, now);
        }
        self.status_now()
    }

    fn phase_now(&self) -> Phase {
        if !self.asked.is_empty() {
            Phase::NeedsYou
        } else if self.running {
            Phase::Working
        } else {
            match &self.last_end {
                Some(TurnState::Complete) => Phase::Done,
                Some(TurnState::Failed { .. }) => Phase::Failed,
                Some(TurnState::Interrupted) => Phase::Stopped,
                Some(TurnState::Active) | None => Phase::Idle,
            }
        }
    }

    fn status_now(&self) -> Action {
        let (phase, since_ms) = self.phase;
        let wait = self
            .asked
            .values()
            .next()
            .filter(|_| phase == Phase::NeedsYou)
            .map(|asked| Wait { kind: "permission".to_owned(), text: asked.title.clone() });
        Action::Status(Status { phase, wait, liveness: Liveness::Live, since_ms })
    }
}

/// What an error the agent answered with says, in words for the person.
#[must_use]
pub fn said(error: &acp::Error) -> String {
    if error.code == acp::ErrorCode::AuthRequired {
        return format!(
            "The agent asks to be signed in ({}). Slopty signs no agent in: sign in with the agent's own command in a terminal, then send again.",
            error.message
        );
    }
    match &error.data {
        Some(Value::String(data)) => format!("{}: {data}", error.message),
        _ => error.message.clone(),
    }
}

fn body_of(part: Part, text: &str) -> ItemBody {
    let clipped = Clipped::head(text, PROSE, None);
    match part {
        Part::Thought => ItemBody::Reasoning(clipped),
        Part::Text | Part::User => ItemBody::Text(clipped),
    }
}

const fn plan_status(status: &acp::PlanEntryStatus) -> &'static str {
    match status {
        acp::PlanEntryStatus::InProgress => "in_progress",
        acp::PlanEntryStatus::Completed => "completed",
        _ => "pending",
    }
}

/// The diff from `old` to `new`, as one hunk without line numbers: the agent sends both
/// texts, not where in the file they are.
fn patch_of(old: &str, new: &str) -> Patch {
    let made = crate::conversation::proposed_patch(&[(old.to_owned(), new.to_owned())]);
    Patch {
        hunks: made
            .hunks
            .into_iter()
            .map(|h| Hunk {
                old_start: h.old_start,
                old_lines: h.old_lines,
                new_start: h.new_start,
                new_lines: h.new_lines,
                heading: h.heading,
                lines: h.lines,
            })
            .collect(),
        added: made.added,
        removed: made.removed,
        clipped_lines: made.clipped_lines,
        full: None,
    }
}

/// A select option's choices, its groups flattened: each value and its name.
fn select_choices(option: &acp::SessionConfigOption) -> Vec<(String, String)> {
    let acp::SessionConfigKind::Select(select) = &option.kind else { return Vec::new() };
    let flat: Vec<&acp::SessionConfigSelectOption> = match &select.options {
        acp::SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
        acp::SessionConfigSelectOptions::Grouped(groups) => {
            groups.iter().flat_map(|g| g.options.iter()).collect()
        }
        _ => Vec::new(),
    };
    flat.into_iter().map(|o| (o.value.0.to_string(), o.name.clone())).collect()
}

fn select_current(option: &acp::SessionConfigOption) -> Option<String> {
    match &option.kind {
        acp::SessionConfigKind::Select(select) => Some(select.current_value.0.to_string()),
        _ => None,
    }
}

const fn is_diff(content: &acp::ToolCallContent) -> bool {
    matches!(content, acp::ToolCallContent::Diff(_))
}

fn select_offers(option: &acp::SessionConfigOption, value: &str) -> bool {
    select_choices(option).iter().any(|(id, _)| id == value)
}

/// A call as the thread shows it, from what the agent last told of it; its state apart.
fn shown(raw: &Told) -> ToolCall {
    let input = raw.raw_input.as_ref().filter(|v| !v.is_null());
    let text =
        |key: &str| input.and_then(|i| i.get(key)).and_then(Value::as_str).map(str::to_owned);
    let path = raw
        .locations
        .first()
        .map(|l| l.path.to_string_lossy().into_owned())
        .or_else(|| text("path"))
        .or_else(|| text("file_path"))
        .unwrap_or_default();
    let diff = raw.content.iter().find_map(|c| match c {
        acp::ToolCallContent::Diff(diff) => Some(diff),
        _ => None,
    });
    let (kind, detail) = match (raw.kind, diff) {
        (_, Some(diff)) => {
            let path = diff.path.to_string_lossy().into_owned();
            let old = diff.old_text.clone().unwrap_or_default();
            let patch = patch_of(&old, &diff.new_text);
            if old.is_empty() {
                // The protocol says a new file has no old text; OpenCode gives it an empty one,
                // which is a whole file written either way.
                let lines = u32::try_from(diff.new_text.lines().count()).unwrap_or(u32::MAX);
                let created = diff.old_text.is_none().then_some(true);
                let write = WriteDetail { path, lines, created, patch };
                (kind::WRITE, Some(ToolDetail::Write(write)))
            } else {
                let edit = EditDetail { path, edits: 1, replace_all: false, patch };
                (kind::EDIT, Some(ToolDetail::Edit(edit)))
            }
        }
        (acp::ToolKind::Execute, None) => {
            let command = input.and_then(|i| i.get("command")).map_or_else(
                || raw.title.clone(),
                |c| match c {
                    Value::String(s) => s.clone(),
                    Value::Array(words) => {
                        words.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
                    }
                    other => other.to_string(),
                },
            );
            let exec = ExecDetail {
                command: Clipped::head(&command, OUTPUT, None),
                description: None,
                cwd: text("cwd"),
                background: false,
                task: None,
                status: ExecStatus::Running,
                exit_code: None,
                stderr: None,
                duration_ms: None,
            };
            (kind::EXEC, Some(ToolDetail::Exec(exec)))
        }
        (acp::ToolKind::Read, None) => {
            let line = raw.locations.first().and_then(|l| l.line).map(u64::from);
            let read =
                ReadDetail { path, offset: line, limit: None, lines: None, total_lines: None };
            (kind::READ, Some(ToolDetail::Read(read)))
        }
        (acp::ToolKind::Search, None) => {
            let pattern = text("pattern").or_else(|| text("query")).unwrap_or_default();
            let search = SearchDetail {
                pattern,
                path: text("path"),
                glob: text("glob"),
                files: None,
                matches: None,
                truncated: false,
            };
            (kind::SEARCH, Some(ToolDetail::Search(search)))
        }
        (acp::ToolKind::Fetch, None) => {
            let fetch = FetchDetail {
                url: text("url").unwrap_or_default(),
                prompt: text("prompt"),
                code: None,
                bytes: None,
            };
            (kind::FETCH, Some(ToolDetail::Fetch(fetch)))
        }
        (acp::ToolKind::Edit, None) => (kind::EDIT, None),
        (acp::ToolKind::Delete, None) => ("delete", None),
        (acp::ToolKind::Move, None) => ("move", None),
        (acp::ToolKind::Think, None) => ("think", None),
        (acp::ToolKind::SwitchMode, None) => ("switch-mode", None),
        _ => (kind::OTHER, None),
    };
    let output = raw
        .content
        .iter()
        .filter_map(|c| match c {
            acp::ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            acp::ToolCallContent::Terminal(terminal) => {
                Some(format!("(terminal {})", terminal.terminal_id.0))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let output = if output.is_empty() {
        match &raw.raw_output {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    } else {
        output
    };
    let name = raw.name.clone().unwrap_or_else(|| {
        serde_json::to_value(raw.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    });
    let input = input.map(Value::to_string).unwrap_or_default();
    let title = if raw.title.is_empty() { name.clone() } else { raw.title.clone() };
    ToolCall {
        name,
        kind: kind.to_owned(),
        title,
        input: Clipped::head(&input, OUTPUT, None),
        state: ToolState::Streaming,
        output: (!output.is_empty()).then(|| Clipped::tail(&output, OUTPUT, None)),
        images: Vec::new(),
        detail,
        child: None,
        ended_ms: None,
    }
}

/// A call's end, when it has ended: its time, and its command's status.
const fn finish(call: &mut ToolCall, now: WallMs) {
    if !call.state.is_final() {
        return;
    }
    call.ended_ms = Some(now);
    if let Some(ToolDetail::Exec(exec)) = call.detail.as_mut() {
        exec.status = match call.state {
            ToolState::Completed => ExecStatus::Done,
            ToolState::Rejected | ToolState::Cancelled => ExecStatus::Interrupted,
            _ => ExecStatus::Failed,
        };
    }
}

/// The tokens a turn took, as the agent's answer to its prompt counts them.
fn usage_of(usage: &acp::Usage) -> thread::Usage {
    let counts = [
        (thread::Usage::INPUT, Some(usage.input_tokens)),
        (thread::Usage::OUTPUT, Some(usage.output_tokens)),
        (thread::Usage::REASONING, usage.thought_tokens),
        (thread::Usage::CACHE_READ, usage.cached_read_tokens),
        (thread::Usage::CACHE_WRITE, usage.cached_write_tokens),
    ];
    thread::Usage(
        counts
            .into_iter()
            .filter_map(|(kind, n)| Some((kind.to_owned(), n.filter(|n| *n > 0)?)))
            .collect(),
    )
}
