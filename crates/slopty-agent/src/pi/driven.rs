//! One pi session, driven over RPC, mapped onto the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Sans-IO. The worker runs pi and carries its records; this turns what pi says into
//! [`Action`]s, and what a client asks of the thread into the [`Command`]s that go to pi. It
//! keeps only what the mapping needs: the turn under way, the message streaming, each call's
//! state, the gate's asks open, and the messages sent and not yet heard back.
//!
//! - **The thread.** A pi session is one thread, its id derived from the session's id
//!   ([`thread_of`]), which the worker gives pi (`--session-id`), so the same session is the same
//!   thread across a worker restart and in the person's own pi.
//! - **Turns.** A message from the person opens the next turn, unless it comes while a run still
//!   works (a steer), which joins the turn under way. A turn ends when pi settles, or when the next
//!   one opens first (a follow-up). The same rule rebuilds the turns from the session's entries
//!   ([`Driven::entries`]), so a thread read again is the thread that was streamed.
//! - **Items.** Messages are numbered in the order the session holds them. The person's message `n`
//!   is item `msg-n`; block `i` of the model's is `msg-n.i`; a tool call keeps pi's id. A message
//!   streams from its deltas and is replaced whole at its end, which is authoritative; its start is
//!   not read, since pi writes it from the message it goes on filling.
//! - **Approvals.** The gate asks about each call ([`GateAsk`]) as a request on the call's item,
//!   with allow, deny, and deny and stop. The worker's answer settles it at once: pi has no other
//!   client to answer it.
//! - **Other dialogs.** Any other extension the person's pi loads may ask too: a choice, a yes or
//!   no, or a text. Each is a question with the answers the extension offers, and stays open until
//!   it is answered or pi gives up on it at its timeout, since pi waits on it whatever the turn
//!   does. A notice an extension shows is a notice in the thread.
//! - **Who holds it.** Slopty drives the session (drive `driven`, no terminal), or pi's own TUI
//!   holds it in a terminal (drive `observed`, the terminal named), when the thread follows the
//!   entries the TUI appends to the session's file ([`Driven::appended`]) and can only be taken
//!   back ([`Driven::held_by_tui`], [`Driven::held_by_slopty`]).

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde_json::Value;
use slopty_core::WallMs;
use slopty_proto::thread::detail::{ExecDetail, ExecStatus, Question, ReadDetail, SearchDetail};
use slopty_proto::thread::{
    self, Action, AgentId, Answerer, AskId, Cap, Changed, Clipped, Compaction, Drive, Effect,
    IntentId, Item, ItemBody, ItemId, Liveness, Meters, Model, Notice, PartKey, Phase,
    Request as Ask, RequestState, Retry, Status, ThreadId, ThreadMeta, ThreadState, ToolCall,
    ToolDetail, ToolState, Turn, TurnId, TurnState, UserMessage, Wait, kind,
};

use super::rpc::{
    self, AssistantEvent, Command, Content, Entries, GateAsk, Incoming, Message, Request, State,
    Stats, StreamingBehavior, ToolOutput, UiMethod, UiRequest,
};
use crate::attach::Attached;
use crate::driven::{OUTPUT, PROSE, caps, choice, micro_usd, title_of, tool};

/// What a driven pi can do through Slopty.
pub const CAPS: [&str; 11] = [
    Cap::APPROVALS,
    Cap::COMPACT,
    Cap::CONTINUE,
    Cap::FORK,
    Cap::HANDOFF,
    Cap::INTERRUPT,
    Cap::LIVE_TEXT,
    Cap::SET_MODEL,
    Cap::SCHEDULE,
    Cap::SLEEP,
    Cap::STEER,
];

/// What a pi whose own TUI holds the session can do through Slopty: be taken back.
pub const TUI_CAPS: [&str; 1] = [Cap::HANDOFF];

/// The fact that keeps the flags a thread's pi was started with, as a JSON list: a pi started
/// again for it, driven or in its TUI, gets them too.
pub const ARGS_FACT: &str = "pi-args";

/// The fact that names the session's file, as pi said it.
pub const SESSION_FILE_FACT: &str = "session-file";

/// The flags a thread's pi was started with ([`ARGS_FACT`]).
#[must_use]
pub fn args_of(meta: &ThreadMeta) -> Vec<String> {
    meta.facts.get(ARGS_FACT).and_then(|args| serde_json::from_str(args).ok()).unwrap_or_default()
}

/// The gate's answers, by their choice ids.
pub const ALLOW: &str = "allow";
/// Deny the call; the turn goes on.
pub const DENY: &str = "deny";
/// Deny the call and stop the turn.
pub const DENY_STOP: &str = "deny-stop";
/// A confirmation's yes.
pub const YES: &str = "yes";
/// A confirmation's no.
pub const NO: &str = "no";

/// What an aborted model call's message says: Node's own wording for an aborted signal, which
/// pi records as the error of an interrupted turn (`stopReason: "error"`), so a session read
/// again knows the turn was stopped, not failed.
const ABORTED: &str = "This operation was aborted";

/// The thread of pi session `session`.
#[must_use]
pub fn thread_of(session: &str) -> ThreadId {
    ThreadId::derived(&["pi session", session])
}

/// Messages sent and not yet heard back that are kept; past it the oldest is given up. One pi
/// rewrites (a template, a skill) never comes back in the words that went.
const SENDS: usize = 64;

/// A dialog open on the thread.
#[derive(Clone, Debug)]
struct Open {
    /// What answers it.
    asks: Asks,
    /// What it asks, in a line.
    title: String,
    /// When pi gives up waiting on it.
    until: Option<WallMs>,
}

/// What answers a dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asks {
    /// The gate, about a call.
    Gate {
        /// The call.
        call: String,
    },
    /// One of these.
    Choice(Vec<String>),
    /// Yes or no.
    Confirm,
    /// Whatever the person writes.
    Text,
}

impl Open {
    fn call(&self) -> Option<&str> {
        match &self.asks {
            Asks::Gate { call } => Some(call),
            _ => None,
        }
    }
}

/// The thread `state` of a pi that ended unheard, as when the worker that ran it stopped: what
/// it was doing is cut short as [`Driven::exited`] cuts it, from what the thread holds.
#[must_use]
pub fn gone(state: &ThreadState, now: WallMs) -> Vec<Action> {
    crate::driven::cut_short(state, true, now)
}

/// What a person's answer to a gate ask comes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    /// What goes to pi: the answer, then an abort when the choice stops the turn.
    pub requests: Vec<Request>,
    /// What the thread shows of it at once.
    pub actions: Vec<Action>,
}

/// A pi session as Slopty shows it.
#[derive(Debug)]
pub struct Driven {
    meta: ThreadMeta,
    /// Turns so far; the last is the one under way when `turn_open`.
    turns: u32,
    turn_open: bool,
    /// The turn under way as it began, told again once its input is known.
    begun: Option<Turn>,
    /// Messages of the session so far, which number its items.
    messages: u32,
    /// The message streaming: its number and the item of each block begun.
    streaming: Option<(u32, BTreeMap<u32, ItemId>)>,
    /// Each call as last told, by pi's id.
    calls: HashMap<String, (TurnId, ToolCall)>,
    /// The dialogs open, by their id.
    open: BTreeMap<AskId, Open>,
    /// Calls the person denied, which end blocked rather than failed.
    denied: Vec<String>,
    /// Messages sent and not yet heard back, with the intent that sent each.
    sends: VecDeque<(IntentId, String)>,
    meters: Meters,
    /// What the turn under way has taken so far.
    usage: thread::Usage,
    /// Whether a run works now.
    running: bool,
    /// Whether an abort was sent for the run under way.
    aborting: bool,
    /// How the last turn ended.
    last_end: Option<TurnState>,
    /// The phase last told, and since when.
    phase: (Phase, WallMs),
    notices: u32,
}

impl Driven {
    /// pi session `session` of pi `version` in `cwd`, with the actions that begin its thread.
    #[must_use]
    pub fn new(session: &str, version: &str, cwd: &str, now: WallMs) -> (Self, Vec<Action>) {
        let meta = ThreadMeta {
            modes: Vec::new(),
            id: thread_of(session),
            agent: AgentId::named(AgentId::PI),
            agent_version: version.to_owned(),
            native: session.to_owned(),
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
        Self::with(meta, now)
    }

    /// The thread `meta` as it stands, read again from nothing: what it is and who holds it are
    /// kept, what it holds is told again by [`Driven::entries`] or [`Driven::observed`].
    #[must_use]
    pub fn of(meta: &ThreadMeta, now: WallMs) -> (Self, Vec<Action>) {
        let mut meta = meta.clone();
        meta.models.clear();
        Self::with(meta, now)
    }

    fn with(meta: ThreadMeta, now: WallMs) -> (Self, Vec<Action>) {
        let driven = Self {
            meta,
            turns: 0,
            turn_open: false,
            begun: None,
            messages: 0,
            streaming: None,
            calls: HashMap::new(),
            open: BTreeMap::new(),
            denied: Vec::new(),
            sends: VecDeque::new(),
            meters: Meters::default(),
            usage: thread::Usage::default(),
            running: false,
            aborting: false,
            last_end: None,
            phase: (Phase::Idle, now),
            notices: 0,
        };
        let actions = vec![Action::Meta(Box::new(driven.meta.clone())), driven.status_now()];
        (driven, actions)
    }

    /// What the thread is.
    #[must_use]
    pub const fn meta(&self) -> &ThreadMeta {
        &self.meta
    }

    /// The flags pi was started with, kept for every pi started again for the thread.
    pub fn started_with(&mut self, args: &[String]) -> Vec<Action> {
        if args.is_empty() {
            return Vec::new();
        }
        let json = serde_json::to_string(args).unwrap_or_default();
        self.meta.facts.insert(ARGS_FACT.to_owned(), json);
        vec![Action::Meta(Box::new(self.meta.clone()))]
    }

    /// This thread's session branched off `fork`'s (`--fork`), which it holds the history of.
    pub fn forked(&mut self, fork: thread::Fork) -> Vec<Action> {
        self.meta.forked_from = Some(fork);
        ThreadMeta::FORK.clone_into(&mut self.meta.origin);
        vec![Action::Meta(Box::new(self.meta.clone()))]
    }

    /// The terminal whose TUI holds the session, when one does.
    #[must_use]
    pub const fn held(&self) -> Option<slopty_core::SessionId> {
        self.meta.terminal
    }

    /// pi's own TUI in `terminal` holds the session from `now`: the thread follows what it
    /// writes, and can only be taken back.
    pub fn held_by_tui(&mut self, terminal: slopty_core::SessionId, now: WallMs) -> Vec<Action> {
        self.meta.terminal = Some(terminal);
        self.meta.drive = Drive::named(Drive::OBSERVED);
        self.meta.caps = caps(&TUI_CAPS);
        let mut actions = self.withdraw(|_| true);
        actions.push(Action::Meta(Box::new(self.meta.clone())));
        self.phase = (self.phase_now(), now);
        actions.push(self.status_now());
        actions
    }

    /// Slopty holds the session again, its TUI gone; the next pi is driven.
    pub fn held_by_slopty(&mut self) -> Vec<Action> {
        self.meta.terminal = None;
        self.meta.drive = Drive::named(Drive::DRIVEN);
        self.meta.caps = caps(&CAPS);
        vec![Action::Meta(Box::new(self.meta.clone()))]
    }

    /// Whether the session rests: no run works and, as its TUI writes it, no turn is open. A
    /// handoff waits for it.
    #[must_use]
    pub const fn rests(&self) -> bool {
        !self.running && !self.turn_open
    }

    /// Whether a run works now.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    /// A record pi wrote, heard at `now`.
    pub fn incoming(&mut self, record: &Incoming, now: WallMs) -> Vec<Action> {
        match record {
            Incoming::AgentStart => {
                self.running = true;
                vec![self.status(now)]
            }
            Incoming::AgentSettled => self.settled(now),
            Incoming::MessageStart { message } => self.message_start(message, now),
            Incoming::MessageUpdate { event, .. } => self.update(event, now),
            Incoming::MessageEnd { message } => self.message_end(message, now),
            Incoming::ToolExecutionStart { tool_call_id, .. } => {
                self.call_state(tool_call_id, ToolState::Running, None, now)
            }
            Incoming::ToolExecutionUpdate { tool_call_id, partial_result } => {
                let Some(output) = partial_result.as_ref() else { return Vec::new() };
                self.call_output(tool_call_id, output, now)
            }
            Incoming::ToolExecutionEnd { tool_call_id, result, is_error } => {
                self.call_ended(tool_call_id, result, *is_error, now)
            }
            Incoming::ExtensionUiRequest(ui) => self.ui(ui, now),
            Incoming::SessionInfoChanged { name } => {
                self.meta.title = name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| self.meta.title.clone());
                vec![Action::Meta(Box::new(self.meta.clone()))]
            }
            Incoming::CompactionEnd { reason, aborted: false, error_message: None, result } => {
                let item = Item {
                    id: ItemId(format!("compaction:{}", self.messages)),
                    turn: self.turn(),
                    at_ms: now,
                    body: ItemBody::Compaction(Compaction {
                        trigger: Some(reason.clone()),
                        before_tokens: result.as_ref().and_then(|r| r.tokens_before),
                        after_tokens: result.as_ref().and_then(|r| r.estimated_tokens_after),
                        summary: result
                            .as_ref()
                            .and_then(|r| r.summary.as_deref())
                            .map(|s| Clipped::head(s, PROSE, None)),
                    }),
                };
                vec![Action::ItemCompleted(item)]
            }
            Incoming::CompactionEnd { error_message: Some(error), .. } => {
                self.notice(Notice::INFO, &format!("Compacting the context failed: {error}"), now)
            }
            Incoming::ThinkingLevelChanged { level } => self.effort(level),
            Incoming::AutoRetryStart { attempt, max_attempts, delay_ms, error_message } => {
                let retry = Retry { attempt: *attempt, max: Some(*max_attempts), in_ms: *delay_ms };
                self.notices = self.notices.saturating_add(1);
                let item = Item {
                    id: ItemId(format!("notice:{}", self.notices)),
                    turn: self.turn(),
                    at_ms: now,
                    body: ItemBody::Notice(Notice {
                        kind: Notice::API_ERROR.to_owned(),
                        text: Clipped::whole(error_message),
                        retry: Some(retry),
                    }),
                };
                vec![Action::ItemCompleted(item)]
            }
            Incoming::AutoRetryEnd { success: false, final_error: Some(error) } => {
                self.notice(Notice::API_ERROR, error, now)
            }
            Incoming::ExtensionError { event, error } => {
                self.notice(Notice::INFO, &format!("An extension failed in {event}: {error}"), now)
            }
            Incoming::Response(response) if !response.success => {
                let error = response.error.as_deref().unwrap_or("no reason given");
                self.notice(Notice::INFO, &format!("pi refused {}: {error}", response.command), now)
            }
            _ => Vec::new(),
        }
    }

    /// The thread rebuilt from the session's entries, on the branch the session is at: what a
    /// thread read again from pi's own record holds. Only for a [`Driven`] that has heard
    /// nothing yet.
    pub fn entries(&mut self, entries: &Entries, now: WallMs) -> Vec<Action> {
        let mut actions = Vec::new();
        for entry in branch(entries) {
            let Some(message) = &entry.message else { continue };
            actions.extend(self.message_start(message, now));
            actions.extend(self.message_end(message, now));
        }
        // A call the session never heard back from did not finish.
        let dangling: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, (_, call))| !call.state.is_final())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dangling {
            actions.extend(self.call_state(&id, ToolState::Cancelled, None, now));
        }
        // Nothing runs while the session is read again: a turn it left open was cut short.
        if self.turn_open {
            if !self.last_message_ended_run() {
                self.last_end = Some(TurnState::Interrupted);
            }
            actions.extend(self.end_turn(now));
        }
        actions.push(self.status(now));
        actions
    }

    /// The thread rebuilt from the session's entries as pi's own TUI goes on writing them: as
    /// [`Driven::entries`], but a turn the TUI is still in stays open and working. Only for a
    /// [`Driven`] that has heard nothing yet.
    pub fn observed(&mut self, entries: &Entries, now: WallMs) -> Vec<Action> {
        let mut actions = Vec::new();
        for entry in branch(entries) {
            actions.extend(self.appended_quietly(entry, now));
        }
        actions.push(self.status(now));
        actions
    }

    /// Entry `entry`, which pi's own TUI appended to the session after the last one heard.
    pub fn appended(&mut self, entry: &rpc::Entry, now: WallMs) -> Vec<Action> {
        let mut actions = self.appended_quietly(entry, now);
        actions.push(self.status(now));
        actions
    }

    fn appended_quietly(&mut self, entry: &rpc::Entry, now: WallMs) -> Vec<Action> {
        let Some(message) = &entry.message else { return Vec::new() };
        let mut actions = self.message_start(message, now);
        actions.extend(self.message_end(message, now));
        // The TUI's run works from the person's message until the model's that ends it.
        match message {
            Message::User { .. } => self.running = true,
            Message::Assistant { .. } if self.last_message_ended_run() => {
                self.running = false;
                actions.extend(self.end_turn(now));
            }
            _ => {}
        }
        actions
    }

    /// The thinking level is `level`, as pi names it.
    fn effort(&mut self, level: &str) -> Vec<Action> {
        let effort = Some(level.to_owned()).filter(|l| !l.is_empty());
        if effort == self.meters.effort {
            return Vec::new();
        }
        self.meters.effort = effort;
        vec![Action::MetersSet(self.meters.clone())]
    }

    /// The session's state, from `get_state`.
    pub fn state(&mut self, state: &State) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(level) = &state.thinking_level {
            actions.extend(self.effort(level));
        }
        if let Some(model) = &state.model {
            self.meters.model = Some(model.name.clone().unwrap_or_else(|| model.id.clone()));
            self.meters.model_id = Some(model_id(&model.provider, &model.id));
            self.meters.context_window = model.context_window;
            actions.push(Action::MetersSet(self.meters.clone()));
        }
        let mut meta = self.meta.clone();
        if let Some(file) = &state.session_file {
            meta.facts.insert(SESSION_FILE_FACT.to_owned(), file.clone());
        }
        if let Some(name) = state.session_name.clone().filter(|n| !n.is_empty()) {
            meta.title = name;
        }
        if meta != self.meta {
            self.meta = meta;
            actions.push(Action::Meta(Box::new(self.meta.clone())));
        }
        actions
    }

    /// The session's tokens and cost, from `get_session_stats`.
    pub fn stats(&mut self, stats: &Stats) -> Vec<Action> {
        let mut meters = self.meters.clone();
        if let Some(context) = stats.context_usage {
            meters.context_tokens = context.tokens;
            meters.context_window = Some(context.context_window);
        }
        meters.cost_micro_usd = Some(micro_usd(stats.cost));
        if meters == self.meters {
            return Vec::new();
        }
        self.meters = meters;
        vec![Action::MetersSet(self.meters.clone())]
    }

    /// The models pi can use, from `get_available_models`.
    pub fn models(&mut self, models: &[rpc::Model]) -> Vec<Action> {
        self.meta.models = models
            .iter()
            .map(|m| Model {
                id: model_id(&m.provider, &m.id),
                label: m.name.clone().unwrap_or_else(|| m.id.clone()),
            })
            .collect();
        vec![Action::Meta(Box::new(self.meta.clone()))]
    }

    /// pi is gone, at `now`, for the reason in `why` when it failed: the thread can be taken up
    /// again from its session. What it was doing is cut short: its dialogs are no longer asked,
    /// its calls are cancelled, and the turn under way ends stopped, or failed with `why`.
    pub fn exited(&mut self, why: Option<&str>, now: WallMs) -> Vec<Action> {
        self.running = false;
        let mut actions = self.withdraw(|_| true);
        let dangling: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, (_, call))| !call.state.is_final())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dangling {
            actions.extend(self.call_state(&id, ToolState::Cancelled, None, now));
        }
        if let Some(why) = why {
            actions.extend(self.notice(Notice::INFO, why, now));
        }
        if self.turn_open && !self.last_message_ended_run() {
            self.last_end = Some(match why {
                Some(why) => TurnState::Failed { error: why.to_owned(), until_ms: None },
                None => TurnState::Interrupted,
            });
        }
        actions.extend(self.end_turn(now));
        self.phase = (self.phase_now(), now);
        actions.push(Action::Status(Status {
            phase: self.phase.0,
            wait: None,
            liveness: Liveness::Exited { resumable: true },
            since_ms: now,
        }));
        actions
    }

    /// What goes to pi for `text`, sent as intent `intent`: a prompt that steers into the run
    /// under way when there is one, and starts one when there is not. pi takes it either way,
    /// so a run that settles as it goes loses nothing.
    ///
    /// The pictures in `attached` go as pi's images; any other file goes by its path in the
    /// words.
    pub fn send(&mut self, text: &str, attached: &[Attached], intent: IntentId) -> Command {
        let message = crate::attach::with_files(text, attached);
        if self.sends.len() >= SENDS {
            self.sends.pop_front();
        }
        self.sends.push_back((intent, message.clone()));
        let images = crate::attach::pictures(attached)
            .filter_map(|picture| match picture {
                Attached::Picture { media_type, .. } => Some(rpc::ImageContent {
                    kind: "image".to_owned(),
                    data: picture.base64()?,
                    mime_type: (*media_type).to_owned(),
                }),
                Attached::File { .. } => None,
            })
            .collect();
        Command::Prompt { message, images, streaming_behavior: Some(StreamingBehavior::Steer) }
    }

    /// The message sent as `intent` did not go: pi refused it.
    pub fn unsent(&mut self, intent: IntentId) {
        self.sends.retain(|(sent, _)| *sent != intent);
    }

    /// When the first dialog open runs out, as pi gives up on it.
    #[must_use]
    pub fn deadline(&self) -> Option<WallMs> {
        self.open.values().filter_map(|open| open.until).min()
    }

    /// The dialogs pi gave up on by `now`, withdrawn.
    pub fn expire(&mut self, now: WallMs) -> Vec<Action> {
        let mut actions = self.withdraw(|open| open.until.is_some_and(|until| until <= now));
        if !actions.is_empty() {
            actions.push(self.status(now));
        }
        actions
    }

    /// What stops the run under way, when one is.
    pub const fn interrupt(&mut self) -> Option<Command> {
        if !self.running {
            return None;
        }
        self.aborting = true;
        Some(Command::Abort)
    }

    /// What switches to model `model` (`provider/id`, as [`Driven::models`] names it).
    #[must_use]
    pub fn set_model(model: &str) -> Option<Command> {
        let (provider, id) = model.split_once('/')?;
        Some(Command::SetModel { provider: provider.to_owned(), model_id: id.to_owned() })
    }

    /// Whether dialog `ask` is open and takes `choice`.
    #[must_use]
    pub fn takes(&self, ask: &AskId, choice: &str) -> bool {
        self.open.get(ask).is_some_and(|open| match &open.asks {
            Asks::Gate { .. } => matches!(choice, ALLOW | DENY | DENY_STOP),
            Asks::Choice(options) => options.iter().any(|o| o == choice),
            Asks::Confirm => matches!(choice, YES | NO),
            Asks::Text => true,
        })
    }

    /// The answer to dialog `ask` with choice `choice` (and, for the gate's deny, the person's
    /// reason in `message`), given by `by` at `now`. `None` when no such dialog is open or it
    /// takes no such answer ([`Driven::takes`]).
    pub fn answer(
        &mut self,
        ask: &AskId,
        choice: &str,
        message: Option<&str>,
        by: Answerer,
        now: WallMs,
    ) -> Option<Answered> {
        if !self.takes(ask, choice) {
            return None;
        }
        let open = self.open.remove(ask)?;
        let mut requests = match (&open.asks, choice) {
            (Asks::Gate { .. }, ALLOW) => vec![rpc::allow(&ask.0)],
            (Asks::Gate { .. }, _) => vec![rpc::deny(&ask.0, message)],
            (Asks::Confirm, _) => vec![rpc::confirm(&ask.0, choice == YES)],
            (Asks::Choice(_) | Asks::Text, _) => vec![rpc::answer(&ask.0, choice.to_owned())],
        };
        let mut actions = vec![Action::RequestResolved {
            id: ask.clone(),
            state: RequestState::Answered { by, choice: choice.to_owned() },
        }];
        if let Asks::Gate { call } = open.asks {
            if choice == ALLOW {
                actions.extend(self.call_state(&call, ToolState::Running, None, now));
            } else {
                self.denied.push(call);
            }
            if choice == DENY_STOP {
                requests.push(Request { id: None, command: Command::Abort });
                self.aborting = true;
            }
        }
        actions.push(self.status(now));
        Some(Answered { requests, actions })
    }

    const fn turn(&self) -> TurnId {
        TurnId(self.turns)
    }

    /// Open the next turn at `now`, ending the one before if it is still open.
    fn open_turn(&mut self, now: WallMs) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.turn_open {
            actions.extend(self.end_turn(now));
        }
        self.turns = self.turns.saturating_add(1);
        self.turn_open = true;
        self.usage = thread::Usage::default();
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
            _ if self.aborting => TurnState::Interrupted,
            Some(TurnState::Active) | None => TurnState::Complete,
            Some(state) => state,
        };
        self.aborting = false;
        self.last_end = Some(state.clone());
        // The gate asks only while its call waits; another extension's dialog outlasts the turn.
        let mut actions = self.withdraw(|open| open.call().is_some());
        actions.push(Action::TurnEnded {
            turn: self.turn(),
            state,
            usage: std::mem::take(&mut self.usage),
            ended_ms: now,
        });
        actions
    }

    /// Whether the session's last message ended a run: the model's, with nothing left to call.
    fn last_message_ended_run(&self) -> bool {
        matches!(&self.last_end, Some(state) if *state != TurnState::Active)
    }

    fn settled(&mut self, now: WallMs) -> Vec<Action> {
        self.running = false;
        let mut actions = self.end_turn(now);
        actions.push(self.status(now));
        actions
    }

    /// The dialogs `which` picks, withdrawn.
    fn withdraw(&mut self, which: impl Fn(&Open) -> bool) -> Vec<Action> {
        let gone: Vec<AskId> =
            self.open.iter().filter(|(_, open)| which(open)).map(|(id, _)| id.clone()).collect();
        gone.into_iter()
            .map(|id| {
                self.open.remove(&id);
                Action::RequestResolved { id, state: RequestState::Withdrawn }
            })
            .collect()
    }

    fn message_start(&mut self, message: &Message, now: WallMs) -> Vec<Action> {
        if matches!(message, Message::Other) {
            return Vec::new();
        }
        self.messages = self.messages.saturating_add(1);
        match message {
            Message::User { .. } => {
                // A message while the run still works steers it; else it opens the next turn.
                if self.turn_open && !self.last_message_ended_run() {
                    Vec::new()
                } else {
                    self.open_turn(now)
                }
            }
            Message::Assistant { .. } => {
                self.streaming = Some((self.messages, BTreeMap::new()));
                Vec::new()
            }
            Message::ToolResult { .. } | Message::Other => Vec::new(),
        }
    }

    fn update(&mut self, event: &AssistantEvent, now: WallMs) -> Vec<Action> {
        let Some((message, blocks)) = self.streaming.as_mut() else { return Vec::new() };
        let message = *message;
        let turn = TurnId(self.turns);
        match event {
            AssistantEvent::TextStart { content_index }
            | AssistantEvent::ThinkingStart { content_index } => {
                let id = block_id(message, *content_index);
                blocks.insert(*content_index, id.clone());
                let body = if matches!(event, AssistantEvent::TextStart { .. }) {
                    ItemBody::Text(Clipped::whole(""))
                } else {
                    ItemBody::Reasoning(Clipped::whole(""))
                };
                vec![Action::ItemStarted(Item { id, turn, at_ms: now, body })]
            }
            AssistantEvent::TextDelta { content_index, delta }
            | AssistantEvent::ThinkingDelta { content_index, delta } => {
                let Some(item) = blocks.get(content_index) else { return Vec::new() };
                vec![Action::Append {
                    item: item.clone(),
                    part: PartKey::Body,
                    text: delta.clone(),
                }]
            }
            AssistantEvent::ToolcallStart { content_index, id, tool_name } => {
                blocks.insert(*content_index, ItemId(id.clone()));
                let call = tool_call(tool_name, &Value::Null, ToolState::Streaming);
                self.calls.insert(id.clone(), (turn, call.clone()));
                let item = Item { id: ItemId(id.clone()), turn, at_ms: now, body: tool(call) };
                vec![Action::ItemStarted(item)]
            }
            AssistantEvent::ToolcallDelta { content_index, delta } => {
                let Some(item) = blocks.get(content_index) else { return Vec::new() };
                vec![Action::Append {
                    item: item.clone(),
                    part: PartKey::Input,
                    text: delta.clone(),
                }]
            }
            AssistantEvent::ToolcallEnd {
                tool_call: Content::ToolCall { id, name, arguments },
                ..
            } => self.call_input(id, name, arguments, now),
            _ => Vec::new(),
        }
    }

    fn message_end(&mut self, message: &Message, now: WallMs) -> Vec<Action> {
        match message {
            Message::User { content } => {
                let text = content.text();
                let intent = self
                    .sends
                    .iter()
                    .position(|(_, sent)| *sent == text)
                    .and_then(|at| self.sends.remove(at))
                    .map(|(intent, _)| intent);
                let id = ItemId(format!("msg-{}", self.messages));
                let mut actions = Vec::new();
                if let Some(begun) = self.begun.as_mut()
                    && begun.input.is_none()
                {
                    begun.input = Some(id.clone());
                    actions.push(Action::TurnStarted(begun.clone()));
                }
                if self.meta.title.is_empty() && !text.trim().is_empty() {
                    self.meta.title = title_of(&text);
                    actions.push(Action::Meta(Box::new(self.meta.clone())));
                }
                let body = ItemBody::User(UserMessage {
                    text: Clipped::head(&text, PROSE, None),
                    images: Vec::new(),
                    command: None,
                    intent,
                });
                actions.push(Action::ItemCompleted(Item {
                    id,
                    turn: self.turn(),
                    at_ms: now,
                    body,
                }));
                actions
            }
            Message::Assistant { content, usage, stop_reason, error_message, model, .. } => {
                let number = self.streaming.take().map_or(self.messages, |(n, _)| n);
                let turn = self.turn();
                let mut actions = Vec::new();
                // The model that wrote it: the turn's own, in place of the session's it began
                // with, and beside another that answered in it before.
                if let Some(model) = model.as_ref().filter(|m| !m.is_empty())
                    && let Some(begun) = self.begun.as_mut()
                    && !begun.models.contains(model)
                {
                    let seeded = begun.models.iter().eq(self.meters.model.iter());
                    if seeded {
                        begun.models.clear();
                    }
                    begun.models.push(model.clone());
                    actions.push(Action::TurnStarted(begun.clone()));
                }
                for (index, block) in (0_u32..).zip(content) {
                    let body = match block {
                        Content::Text { text } => ItemBody::Text(Clipped::head(text, PROSE, None)),
                        Content::Thinking { thinking } => {
                            ItemBody::Reasoning(Clipped::head(thinking, PROSE, None))
                        }
                        Content::ToolCall { id, name, arguments } => {
                            actions.extend(self.call_input(id, name, arguments, now));
                            continue;
                        }
                        Content::Image { .. } | Content::Other => continue,
                    };
                    let item = Item { id: block_id(number, index), turn, at_ms: now, body };
                    actions.push(Action::ItemCompleted(item));
                }
                if let Some(usage) = usage {
                    self.usage.add(&usage_of(usage));
                }
                self.last_end = match stop_reason.as_deref() {
                    Some("toolUse" | "pending") | None => Some(TurnState::Active),
                    Some("aborted") => Some(TurnState::Interrupted),
                    Some("error") if self.aborting || error_message.as_deref() == Some(ABORTED) => {
                        Some(TurnState::Interrupted)
                    }
                    Some("error") => Some(TurnState::Failed {
                        error: error_message.clone().unwrap_or_default(),
                        until_ms: None,
                    }),
                    Some(_) => Some(TurnState::Complete),
                };
                if let Some(TurnState::Failed { error, .. }) = &self.last_end
                    && !error.is_empty()
                {
                    let error = error.clone();
                    actions.extend(self.notice(Notice::API_ERROR, &error, now));
                }
                actions
            }
            Message::ToolResult { tool_call_id, content, is_error, .. } => {
                // Live, the call's end said it all; read again, this is where its end is.
                if self.calls.get(tool_call_id).is_some_and(|(_, call)| call.state.is_final()) {
                    return Vec::new();
                }
                let output = ToolOutput { content: content.clone(), structured: None };
                self.call_ended(tool_call_id, &output, *is_error, now)
            }
            Message::Other => Vec::new(),
        }
    }

    fn ui(&mut self, ui: &UiRequest, now: WallMs) -> Vec<Action> {
        if let Some(gate) = ui.gate() {
            return self.gate(&ui.id, &gate, now);
        }
        let ask = AskId(ui.id.clone());
        let until = |timeout: &Option<u64>| {
            timeout.map(|ms| now.saturating_add(std::time::Duration::from_millis(ms)))
        };
        let (asks, title, text, options, questions, until) = match &ui.method {
            UiMethod::Select { title, options, timeout } => {
                let offered = options.iter().map(|o| choice(o, o, Effect::Answer, false)).collect();
                (Asks::Choice(options.clone()), title, None, offered, Vec::new(), until(timeout))
            }
            UiMethod::Confirm { title, message, timeout } => {
                let offered = vec![
                    choice(YES, "Yes", Effect::Answer, false),
                    choice(NO, "No", Effect::Answer, false),
                ];
                (Asks::Confirm, title, message.as_deref(), offered, Vec::new(), until(timeout))
            }
            UiMethod::Input { title, timeout, .. } => {
                (Asks::Text, title, None, Vec::new(), vec![written(title)], until(timeout))
            }
            UiMethod::Editor { title, prefill, timeout } => {
                let text = prefill.as_deref();
                (Asks::Text, title, text, Vec::new(), vec![written(title)], until(timeout))
            }
            UiMethod::Notify { message, .. } => return self.notice(Notice::INFO, message, now),
            UiMethod::Other => return Vec::new(),
        };
        self.open.insert(ask.clone(), Open { asks, title: title.clone(), until });
        let request = Ask {
            editable: Vec::new(),
            id: ask,
            item: None,
            kind: Ask::QUESTION.to_owned(),
            title: title.clone(),
            text: text.map(|t| Clipped::head(t, PROSE, None)),
            options,
            questions,
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: until,
        };
        vec![Action::RequestOpened(Box::new(request)), self.status(now)]
    }

    /// The gate asks about `gate`'s call, as dialog `id`.
    fn gate(&mut self, id: &str, gate: &GateAsk, now: WallMs) -> Vec<Action> {
        let ask = AskId(id.to_owned());
        let title = ask_title(gate);
        let asks = Asks::Gate { call: gate.call.clone() };
        self.open.insert(ask.clone(), Open { asks, title: title.clone(), until: None });
        let request = Ask {
            editable: Vec::new(),
            id: ask.clone(),
            item: Some(ItemId(gate.call.clone())),
            kind: Ask::APPROVAL.to_owned(),
            title,
            text: None,
            options: vec![
                choice(ALLOW, "Allow", Effect::Allow, false),
                choice(DENY, "Deny", Effect::Deny, false),
                choice(DENY_STOP, "Deny and stop", Effect::Deny, true),
            ],
            questions: Vec::new(),
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: None,
        };
        let mut actions = vec![Action::RequestOpened(Box::new(request))];
        actions.extend(self.call_state(&gate.call, ToolState::Pending { ask }, None, now));
        actions.push(self.status(now));
        actions
    }

    /// Call `id`'s input is whole.
    fn call_input(&mut self, id: &str, name: &str, arguments: &Value, now: WallMs) -> Vec<Action> {
        let turn = self.turn();
        let state = self.calls.get(id).map_or(ToolState::Streaming, |(_, call)| call.state.clone());
        let mut call = tool_call(name, arguments, state);
        if let Some((_, old)) = self.calls.get(id) {
            call.output.clone_from(&old.output);
        }
        let started = self.calls.contains_key(id);
        self.calls.insert(id.to_owned(), (turn, call.clone()));
        let item = Item { id: ItemId(id.to_owned()), turn, at_ms: now, body: tool(call) };
        vec![if started { Action::ItemUpdated(item) } else { Action::ItemStarted(item) }]
    }

    fn call_state(
        &mut self,
        id: &str,
        state: ToolState,
        output: Option<Clipped>,
        now: WallMs,
    ) -> Vec<Action> {
        let Some((turn, call)) = self.calls.get_mut(id) else { return Vec::new() };
        // A call that waits on the person stays so until the person answers.
        let waits = matches!(call.state, ToolState::Pending { .. })
            && state == ToolState::Running
            && self.open.values().any(|open| open.call() == Some(id));
        if !call.state.may_become(&state) || waits {
            return Vec::new();
        }
        call.state = state;
        if output.is_some() {
            call.output = output;
        }
        if call.state.is_final() {
            call.ended_ms = Some(now);
            if let Some(ToolDetail::Exec(exec)) = call.detail.as_mut() {
                exec.status = match call.state {
                    ToolState::Completed => ExecStatus::Done,
                    ToolState::Rejected | ToolState::Cancelled => ExecStatus::Interrupted,
                    _ => ExecStatus::Failed,
                };
            }
        }
        let item =
            Item { id: ItemId(id.to_owned()), turn: *turn, at_ms: now, body: tool(call.clone()) };
        vec![if call.state.is_final() {
            Action::ItemCompleted(item)
        } else {
            Action::ItemUpdated(item)
        }]
    }

    fn call_output(&mut self, id: &str, output: &ToolOutput, now: WallMs) -> Vec<Action> {
        let Some((turn, call)) = self.calls.get_mut(id) else { return Vec::new() };
        let text = rpc::text_of(&output.content);
        if text.is_empty() {
            return Vec::new();
        }
        call.output = Some(Clipped::tail(&text, OUTPUT, None));
        let item =
            Item { id: ItemId(id.to_owned()), turn: *turn, at_ms: now, body: tool(call.clone()) };
        vec![Action::ItemUpdated(item)]
    }

    fn call_ended(
        &mut self,
        id: &str,
        result: &ToolOutput,
        is_error: bool,
        now: WallMs,
    ) -> Vec<Action> {
        let state = if self.denied.iter().any(|d| d == id) {
            ToolState::Rejected
        } else if is_error && self.aborting {
            ToolState::Cancelled
        } else if is_error {
            ToolState::Failed
        } else {
            ToolState::Completed
        };
        let mut actions = Vec::new();
        // An ask about a call that ended is no longer asked.
        actions.extend(self.withdraw(|open| open.call() == Some(id)));
        let text = rpc::text_of(&result.content);
        let output = (!text.is_empty()).then(|| Clipped::tail(&text, OUTPUT, None));
        if let Some((_, call)) = self.calls.get_mut(id)
            && let Some(ToolDetail::Exec(exec)) = call.detail.as_mut()
        {
            exec.exit_code = result
                .structured
                .as_ref()
                .and_then(|s| s.get("exit_code"))
                .and_then(Value::as_i64)
                .and_then(|c| i32::try_from(c).ok());
        }
        actions.extend(self.call_state(id, state, output, now));
        actions.push(self.status(now));
        actions
    }

    fn notice(&mut self, kind: &str, text: &str, now: WallMs) -> Vec<Action> {
        self.notices = self.notices.saturating_add(1);
        let item = Item {
            id: ItemId(format!("notice:{}", self.notices)),
            turn: self.turn(),
            at_ms: now,
            body: ItemBody::Notice(Notice::new(kind, Clipped::whole(text))),
        };
        vec![Action::ItemCompleted(item)]
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
        if !self.open.is_empty() {
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
        let wait = self.open.values().next().filter(|_| phase == Phase::NeedsYou).map(|open| {
            let kind = if open.call().is_some() { "permission" } else { "question" };
            Wait { kind: kind.to_owned(), text: open.title.clone() }
        });
        Action::Status(Status { phase, wait, liveness: Liveness::Live, since_ms })
    }
}

/// A question answered in the person's own words.
fn written(title: &str) -> Question {
    Question { text: title.to_owned(), header: None, options: Vec::new(), multi_select: false }
}

/// The entries on the branch the session is at, oldest first.
fn branch(entries: &Entries) -> Vec<&rpc::Entry> {
    let by_id: HashMap<&str, &rpc::Entry> =
        entries.entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut branch = Vec::new();
    let mut at =
        entries.leaf_id.as_deref().or_else(|| entries.entries.last().map(|e| e.id.as_str()));
    while let Some(entry) = at.and_then(|id| by_id.get(id)) {
        branch.push(*entry);
        at = entry.parent_id.as_deref();
        if branch.len() > entries.entries.len() {
            break;
        }
    }
    branch.reverse();
    branch
}

/// The item of block `index` of message `message`.
fn block_id(message: u32, index: u32) -> ItemId {
    ItemId(format!("msg-{message}.{index}"))
}

/// A model's id as Slopty names it: `provider/id`, which [`Driven::set_model`] takes apart.
fn model_id(provider: &str, id: &str) -> String {
    format!("{provider}/{id}")
}

/// What a gate ask asks, in a line.
fn ask_title(ask: &GateAsk) -> String {
    let call = tool_call(&ask.tool, &ask.input, ToolState::Streaming);
    format!("{}?", call.title)
}

/// A call of pi's tool `name` with `arguments`, typed for the built-in tools.
fn tool_call(name: &str, arguments: &Value, state: ToolState) -> ToolCall {
    let text = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |key: &str| arguments.get(key).and_then(Value::as_u64);
    let path = text("path").or_else(|| text("file_path")).unwrap_or_default();
    let (kind, title, detail) = match name {
        "bash" | "powershell" => {
            let command = text("command").unwrap_or_default();
            let exec = ExecDetail {
                command: Clipped::head(&command, OUTPUT, None),
                description: None,
                cwd: None,
                background: false,
                task: None,
                status: ExecStatus::Running,
                exit_code: None,
                stderr: None,
                duration_ms: None,
            };
            let title = if command.is_empty() {
                "Run a command".to_owned()
            } else {
                format!("Run {command}")
            };
            (kind::EXEC, title, Some(ToolDetail::Exec(exec)))
        }
        "read" => {
            let read = ReadDetail {
                path: path.clone(),
                offset: number("offset"),
                limit: number("limit"),
                lines: None,
                total_lines: None,
            };
            (kind::READ, format!("Read {path}"), Some(ToolDetail::Read(read)))
        }
        "edit" => (kind::EDIT, format!("Edit {path}"), None),
        "write" => (kind::WRITE, format!("Write {path}"), None),
        "grep" | "find" | "ls" => {
            let pattern = text("pattern").unwrap_or_default();
            let search = SearchDetail {
                pattern: pattern.clone(),
                path: text("path"),
                glob: text("glob"),
                files: None,
                matches: None,
                truncated: false,
            };
            let title = if pattern.is_empty() {
                format!("List {path}")
            } else {
                format!("Search for {pattern}")
            };
            (kind::SEARCH, title, Some(ToolDetail::Search(search)))
        }
        other => ("", other.to_owned(), None),
    };
    let input = if arguments.is_null() { String::new() } else { arguments.to_string() };
    ToolCall {
        name: name.to_owned(),
        kind: kind.to_owned(),
        title,
        input: Clipped::head(&input, OUTPUT, None),
        state,
        output: None,
        images: Vec::new(),
        detail,
        child: None,
        ended_ms: None,
    }
}

fn usage_of(usage: &rpc::Usage) -> thread::Usage {
    let mut tokens = BTreeMap::new();
    let mut put = |kind: &str, n: u64| {
        if n > 0 {
            tokens.insert(kind.to_owned(), n);
        }
    };
    put(thread::Usage::INPUT, usage.input);
    put(thread::Usage::CACHE_READ, usage.cache_read);
    put(thread::Usage::CACHE_WRITE, usage.cache_write);
    put(thread::Usage::OUTPUT, usage.output);
    put(thread::Usage::REASONING, usage.reasoning.unwrap_or_default());
    put(thread::Usage::COST_MICRO_USD, micro_usd(usage.cost.total));
    thread::Usage(tokens)
}
