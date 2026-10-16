//! One Claude Code session, driven over its stream-json control protocol, mapped onto the
//! agent-neutral thread model (`slopty_proto::thread`).
//!
//! Sans-IO. The worker runs `claude -p` with stream-json both ways ([`super::args`]) and carries
//! its lines; this turns what Claude Code says into [`Out`]s and what a client asks of the thread
//! into lines for its stdin ([`Driven::take_writes`]). The conversation itself is mapped by
//! [`History`] through the transcript decoder: the messages Claude Code streams are the records
//! it writes to its transcript, under the same ids, so a thread heard live and one read again
//! from the transcript are the same thread.
//!
//! - **Delivery.** A message goes as a user message under an id of its own (the intent's), and is
//!   the person's item once Claude Code echoes it back (`--replay-user-messages`) or says it queued
//!   or began it (`command_lifecycle`). Until then it is on its way in the thread's pending list;
//!   one not acknowledged in [`DELIVERY`] goes again under the same id, which Claude Code takes
//!   once, and one not acknowledged after that, or when Claude Code ends first, is held as one that
//!   may not have reached it ([`Pending::UNKNOWN`]).
//! - **Turns.** A turn begins with the engine's word that it began one, and ends with its result:
//!   complete, interrupted or failed. Work the engine takes up alone (a background task's notice)
//!   runs between them as it does in Claude Code.
//! - **Interrupts.** A stop is a control request; one Claude Code does not acknowledge in
//!   [`INTERRUPT_ACK`] is escalated: the worker ends the process, the turn is cut, and the next
//!   message takes the session up again.
//! - **Approvals.** Claude Code asks about each tool call it may not run alone
//!   (`--permission-prompt-tool=stdio`): a request on the call's item, answered with the decision
//!   it takes, by the request's own id. An `initialize` answer names those still open, so a worker
//!   that comes back to a running Claude Code asks them again.
//! - **Waits.** Compacting, the plan's usage limit (messages are held until it resets, then go), a
//!   stop under way, and a turn that shows no progress for [`STALL`] are said as waits; a gap
//!   between ticks longer than [`WAKE_GAP`] is a machine waking, never a stall.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use serde_json::{Value, json};
use slopty_core::WallMs;
use slopty_proto::thread::{
    Answerer, AskId, Cap, Delivery, Effort, IntentId, Liveness, Meters, Mode, Model, Notice,
    OptionChoice, OptionOffer, Pending, PendingState, Phase, ThreadMeta, ThreadState, Wait,
};

use super::history::{End, History, Out};
use crate::attach::Attached;
use crate::conversation::{
    self as conv, Conversation, Live, LiveId, LiveKind, PermissionEvent, PermissionPrompt, Settled,
    Verdict,
};
use crate::queue::Queue;

/// What a driven Claude Code can do through Slopty.
pub const CAPS: [&str; 12] = [
    Cap::APPROVALS,
    Cap::END,
    Cap::FORK,
    Cap::INTERRUPT,
    Cap::QUEUE,
    Cap::SCHEDULE,
    Cap::SET_EFFORT,
    Cap::SET_MODE,
    Cap::SET_MODEL,
    Cap::SET_OPTION,
    Cap::SNAPSHOTS,
    Cap::STEER,
];

/// How long a message may go unacknowledged before it is sent again, and then before it is held
/// as one that may not have arrived.
pub const DELIVERY: Duration = Duration::from_secs(120);

/// How long a stop may go unacknowledged before the worker ends the process.
pub const INTERRUPT_ACK: Duration = Duration::from_millis(1500);

/// How long a turn may show no progress before it is said to have stalled.
pub const STALL: Duration = Duration::from_mins(16);

/// How often the worker looks at a running turn's progress.
pub const TICK: Duration = Duration::from_secs(60);

/// A gap between two looks longer than this is the machine waking from sleep: the clock jumped,
/// the turn did not stall.
pub const WAKE_GAP: Duration = Duration::from_secs(90);

/// How long after its reset a usage limit's held messages go.
pub const LIMIT_SLACK: Duration = Duration::from_secs(2);

/// The fact that keeps the flags a thread's Claude Code was started with, as a JSON list: one
/// started again for it gets them too.
pub const ARGS_FACT: &str = "claude-args";

/// The fact that names the ptyd session holding the thread's Claude Code, while one does.
pub const CUSTODY_FACT: &str = "claude-custody";

/// The fact that keeps the fast mode chosen for the thread ([`super::FAST`]).
pub const FAST_FACT: &str = "claude-fast";

/// The words a thread's stop is said with while it is under way.
pub const STOPPING_TEXT: &str = "Stopping";

/// The words a turn with no progress is said with.
pub const STALLED_TEXT: &str = "No progress for a while";

/// What a message held until a usage limit resets says.
pub const HELD_FOR_LIMIT: &str = "Held until the usage limit resets";

/// How a slash command's output begins, as Claude Code records it.
const COMMAND_STDOUT: &str = "<local-command-stdout>";

/// The id of the user message that carries the message sent as `intent`.
#[must_use]
pub fn message_id(intent: IntentId) -> String {
    intent.to_string()
}

/// A control request Slopty sent, by what its answer is read as.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Control {
    Initialize,
    Interrupt,
    SetModel(String),
    SetMode(String),
    Effort(String),
    Fast(bool),
    StopTask,
    ContextUsage,
}

/// A message given to Claude Code and not yet acknowledged.
#[derive(Clone, Debug)]
struct Sent {
    /// The user message's id.
    uuid: String,
    /// What it says, as it went.
    pending: Pending,
    /// The line that carried it, sent again when it is not acknowledged.
    line: String,
    /// When it went last.
    at: WallMs,
    /// Whether it went again already.
    again: bool,
}

/// A permission Claude Code asks for, open.
#[derive(Clone, Debug)]
struct Asked {
    /// The control request's id, which the answer names.
    request: String,
    tool: String,
    input: Value,
    suggestions: Value,
}

/// What a client's answer to a request comes to.
#[derive(Debug)]
pub struct Answered {
    /// What the thread shows of it at once.
    pub outs: Vec<Out>,
}

/// A Claude Code session as Slopty shows it and drives it.
#[derive(Debug)]
pub struct Driven {
    history: History,
    decoder: Conversation,
    /// Control requests sent and not answered, by their ids.
    controls: HashMap<String, Control>,
    next: u64,
    /// Lines for Claude Code's stdin, oldest first.
    writes: Vec<String>,
    /// Messages given and not acknowledged, oldest first.
    sends: VecDeque<Sent>,
    /// Messages held: queued until the turn ends, or held until the usage limit resets.
    queued: Queue,
    /// Permissions asked, by the request's number in the thread.
    asks: BTreeMap<u64, Asked>,
    next_ask: u64,
    /// The blocks the model is writing, in each message, by the message's id, in their order,
    /// with whether each is settled.
    blocks: HashMap<String, Vec<(LiveId, conv::ThreadId, bool)>>,
    /// The message each thread streams now (the main one's by `None`).
    streaming: HashMap<Option<String>, String>,
    /// Which subagent each `Agent` call started, by the call.
    agents: HashMap<String, String>,
    /// A turn is under way.
    running: bool,
    /// A stop was sent and not acknowledged, since then.
    interrupting: Option<WallMs>,
    /// The person's stop holds what is queued.
    stopped: bool,
    /// The process is being ended, since then.
    ending: Option<WallMs>,
    /// The usage limit holds messages until then.
    limit_until: Option<WallMs>,
    /// When Claude Code last said anything, and when the worker last looked.
    heard: WallMs,
    looked: Option<WallMs>,
    stalled: bool,
    /// The models Claude Code offers, as `initialize` said, with whether each has fast mode.
    fast_models: Vec<String>,
    version: String,
    /// The outs its beginning gave, until drained.
    begun: Vec<Out>,
    /// The message Claude Code began and has not echoed yet, and what waits for its echo.
    awaiting_echo: Option<String>,
    held: Vec<Value>,
    /// The last user message heard.
    last_echo: Option<String>,
    /// A compaction was said, and its summary is still to come.
    compacted: bool,
}

impl Driven {
    /// Session `session` in `cwd`, begun at `now`.
    #[must_use]
    pub fn new(session: &str, cwd: &str, now: WallMs) -> Self {
        Self::with(History::new(session, "", cwd, &CAPS, now), now)
    }

    /// The thread `state` as it stands, to be read again from its session's records
    /// ([`Self::replay`]) or driven on: what it is and the messages it holds are kept.
    #[must_use]
    pub fn of(state: &ThreadState, now: WallMs) -> Self {
        let mut meta = state.meta.clone();
        meta.caps.clear();
        let mut driven = Self::with(History::of(meta, &CAPS), now);
        driven.queued = Queue::of(
            &state
                .pending
                .iter()
                .filter(|p| p.state != PendingState::Sending && !p.is_unknown())
                .cloned()
                .collect::<Vec<_>>(),
        );
        // What was on its way when the thread was read last: acknowledged by its echo should
        // that come, else held as one that may not have arrived.
        for pending in state.pending.iter().filter(|p| p.state == PendingState::Sending) {
            let uuid = message_id(pending.intent);
            driven.history.sent_as(&uuid, pending.intent);
            driven.sends.push_back(Sent {
                uuid,
                pending: pending.clone(),
                line: String::new(),
                at: now,
                again: true,
            });
        }
        driven
    }

    fn with(mut history: History, now: WallMs) -> Self {
        let begun = history.status(Phase::Idle, None, Liveness::Live, now);
        Self {
            history,
            decoder: Conversation::default(),
            controls: HashMap::new(),
            next: 0,
            writes: Vec::new(),
            sends: VecDeque::new(),
            queued: Queue::default(),
            asks: BTreeMap::new(),
            next_ask: 0,
            blocks: HashMap::new(),
            streaming: HashMap::new(),
            agents: HashMap::new(),
            running: false,
            interrupting: None,
            stopped: false,
            ending: None,
            limit_until: None,
            heard: now,
            looked: None,
            stalled: false,
            fast_models: Vec::new(),
            version: String::new(),
            begun,
            awaiting_echo: None,
            held: Vec::new(),
            last_echo: None,
            compacted: false,
        }
    }

    /// What the thread is.
    #[must_use]
    pub const fn meta(&self) -> &ThreadMeta {
        self.history.meta()
    }

    /// The thread's outs not taken yet: at first, its beginning.
    pub fn drain(&mut self) -> Vec<Out> {
        let mut outs = std::mem::take(&mut self.begun);
        outs.extend(self.history.drain());
        outs
    }

    /// The lines for Claude Code's stdin since last asked, each without its newline.
    pub fn take_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.writes)
    }

    /// Whether a turn is under way, or a message is on its way and about to start one.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.running || !self.sends.is_empty()
    }

    /// The messages held.
    pub const fn queue(&mut self) -> &mut Queue {
        &mut self.queued
    }

    /// Keep `value` as the thread's fact `name`, or forget it.
    pub fn fact(&mut self, name: &str, value: Option<&str>) -> Vec<Out> {
        self.history.fact(name, value)
    }

    /// The flags Claude Code was started with, kept for every one started again for the thread.
    pub fn started_with(&mut self, args: &[String]) -> Vec<Out> {
        let json = serde_json::to_string(args).unwrap_or_default();
        self.history.fact(ARGS_FACT, (!args.is_empty()).then_some(json.as_str()))
    }

    /// The thread branched off `fork`.
    pub fn forked(&mut self, fork: slopty_proto::thread::Fork) -> Vec<Out> {
        self.history.forked(fork)
    }

    /// The thread read again from its session's `records` (the main transcript's, then each
    /// subagent's), at `now`. Only for a [`Driven`] that has heard nothing yet. What the records
    /// leave open ends there unless `running`: a reattached Claude Code still at its turn.
    pub fn replay(&mut self, records: &[Value], running: bool, now: WallMs) -> Vec<Out> {
        let mut outs = self.drain();
        for record in records {
            let changes = self.decoder.ingest(&conv::ThreadId::Main, record);
            outs.extend(self.history.transcript(&changes, &[]));
        }
        if let Some(branch) = super::history::branch_in(records) {
            outs.extend(self.history.fact(super::history::BRANCH_FACT, Some(branch)));
        }
        if !running {
            outs.extend(self.history.rest(now));
        }
        outs.extend(self.shown());
        outs
    }

    /// The branch checked out where Claude Code works is `branch` now (none outside a
    /// repository).
    pub fn branch(&mut self, branch: Option<&str>) -> Vec<Out> {
        self.history.fact(super::history::BRANCH_FACT, branch)
    }

    /// The `initialize` request, which goes first to every Claude Code this drives, and again to
    /// one the worker comes back to.
    pub fn initialize(&mut self) {
        self.control(Control::Initialize, &json!({ "subtype": "initialize" }));
    }

    fn control(&mut self, control: Control, request: &Value) {
        self.next = self.next.saturating_add(1);
        let id = format!("slopty-{}", self.next);
        let line = json!({ "type": "control_request", "request_id": id, "request": request });
        self.controls.insert(id, control);
        self.writes.push(line.to_string());
    }

    /// What the thread's pending list shows: what is on its way, then what is held.
    fn shown(&mut self) -> Vec<Out> {
        let mut pending: Vec<Pending> = self.sends.iter().map(|s| s.pending.clone()).collect();
        if let slopty_proto::thread::Action::PendingSet(held) = self.queued.shown() {
            pending.extend(held);
        }
        self.history.pending(pending)
    }

    /// Send `text` and the files in `attached` as intent `intent`, at `now`: into the turn under
    /// way, which Claude Code takes at its next step, or as a turn of its own. Held instead while
    /// the usage limit holds messages.
    pub fn send(
        &mut self,
        text: &str,
        attached: &[Attached],
        intent: IntentId,
        now: WallMs,
    ) -> Vec<Out> {
        if self.limit_until.is_some() {
            let paths = attached.iter().map(|a| a.path().to_owned()).collect();
            let _shown = self.queued.hold(intent, text, paths, (), false);
            return self.held_for_limit();
        }
        let content = content_of(text, attached);
        let uuid = message_id(intent);
        self.history.sent_as(&uuid, intent);
        let pending = Pending {
            intent,
            text: text.to_owned(),
            attachments: attached.iter().map(|a| a.path().to_owned()).collect(),
            delivery: Delivery::Steer,
            state: PendingState::Sending,
        };
        self.give(uuid, pending, &content, now)
    }

    /// Hand Slopty's reports, `context` as the server framed them ([`crate::reports::TAG`]), to
    /// Claude Code as a message of their own, at `now`. The message's id, which its
    /// acknowledgement names.
    pub fn report(&mut self, context: &str, now: WallMs) -> (String, Vec<Out>) {
        let uuid = uuid::Uuid::new_v4().to_string();
        let text = context.to_owned();
        let pending = Pending {
            intent: IntentId::new(),
            text: text.clone(),
            attachments: Vec::new(),
            delivery: Delivery::Steer,
            state: PendingState::Sending,
        };
        let outs =
            self.give(uuid.clone(), pending, &json!([{ "type": "text", "text": text }]), now);
        (uuid, outs)
    }

    fn give(&mut self, uuid: String, pending: Pending, content: &Value, now: WallMs) -> Vec<Out> {
        let line = json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "parent_tool_use_id": null,
            "session_id": self.history.meta().native,
            "uuid": uuid,
        })
        .to_string();
        self.writes.push(line.clone());
        self.sends.push_back(Sent { uuid, pending, line, at: now, again: false });
        self.stopped = false;
        self.shown()
    }

    /// The message sent as `uuid` reached Claude Code.
    fn acknowledged(&mut self, uuid: &str) -> Option<Sent> {
        let at = self.sends.iter().position(|s| s.uuid == uuid)?;
        self.sends.remove(at)
    }

    /// The reports message `uuid` reached Claude Code: whether it was one.
    #[must_use]
    pub fn reached(&self, uuid: &str) -> bool {
        !self.sends.iter().any(|s| s.uuid == uuid)
    }

    /// Hold `text` until the turn under way ends ([`Cap::QUEUE`]), as intent `intent`.
    pub fn hold(&mut self, intent: IntentId, text: &str, attachments: Vec<String>) -> Vec<Out> {
        let _shown = self.queued.hold(intent, text, attachments, (), false);
        self.shown()
    }

    /// The pending list changed by `change` to the queue.
    pub fn queue_changed(&mut self) -> Vec<Out> {
        self.shown()
    }

    /// The next message held, taken off the queue, once Claude Code has nothing else to do and
    /// nothing holds it.
    pub fn next_queued(&mut self) -> Option<Pending> {
        if self.busy() || self.stopped || self.limit_until.is_some() {
            return None;
        }
        self.queued.next_up().map(|(next, ())| next)
    }

    /// Stop the turn under way, at `now`: nothing queued goes on its own until the person speaks
    /// again. `None` when no turn runs.
    pub fn interrupt(&mut self, now: WallMs) -> Vec<Out> {
        self.stopped = true;
        let _held = self.queued.stop();
        let mut outs = self.shown();
        if !self.running {
            return outs;
        }
        if self.interrupting.is_none() {
            self.interrupting = Some(now);
            self.control(Control::Interrupt, &json!({ "subtype": "interrupt" }));
        }
        outs.extend(self.history.stopping(Some(STOPPING_TEXT.to_owned())));
        outs
    }

    /// Stop subagent `agent`'s run.
    pub fn stop_task(&mut self, agent: &str) {
        self.control(Control::StopTask, &json!({ "subtype": "stop_task", "task_id": agent }));
    }

    /// Switch to model `model`.
    pub fn set_model(&mut self, model: &str) {
        let request = json!({ "subtype": "set_model", "model": model });
        self.control(Control::SetModel(model.to_owned()), &request);
    }

    /// Switch to permission mode `mode`.
    pub fn set_mode(&mut self, mode: &str) {
        let request = json!({ "subtype": "set_permission_mode", "mode": mode });
        self.control(Control::SetMode(mode.to_owned()), &request);
    }

    /// Think at effort `effort`.
    pub fn set_effort(&mut self, effort: &str) {
        let request =
            json!({ "subtype": "apply_flag_settings", "settings": { "effortLevel": effort } });
        self.control(Control::Effort(effort.to_owned()), &request);
    }

    /// Set option `option` of the model to `choice`; `false` for one Claude Code does not offer.
    pub fn set_option(&mut self, option: &str, choice: &str) -> bool {
        if option != super::FAST || !matches!(choice, "on" | "off") {
            return false;
        }
        let on = choice == "on";
        let request = json!({ "subtype": "apply_flag_settings", "settings": { "fastMode": on } });
        self.control(Control::Fast(on), &request);
        true
    }

    /// Whether request `ask` is open and takes `choice`.
    #[must_use]
    pub fn takes(&self, ask: &AskId, choice: &str) -> bool {
        let Some(asked) = ask.0.parse::<u64>().ok().and_then(|n| self.asks.get(&n)) else {
            return false;
        };
        let verdict = super::history::verdict(choice, None);
        match verdict {
            Some(Verdict::Answer { .. }) => asked.tool == "AskUserQuestion",
            Some(_) => true,
            None => false,
        }
    }

    /// The answer `choice` (and the person's `message`) to request `ask`, given by `by` at `now`;
    /// `None` when no such request is open or it takes no such answer.
    pub fn answer(
        &mut self,
        ask: &AskId,
        choice: &str,
        message: Option<&str>,
        by: Answerer,
        _now: WallMs,
    ) -> Option<Answered> {
        if !self.takes(ask, choice) {
            return None;
        }
        let number = ask.0.parse::<u64>().ok()?;
        let asked = self.asks.remove(&number)?;
        let verdict = super::history::verdict(choice, message)?;
        let decision = crate::permission::decision(&verdict, &asked.input, &asked.suggestions);
        let response = crate::permission::result(&decision);
        let line = json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": asked.request, "response": response },
        });
        self.writes.push(line.to_string());
        let settled = Settled::Answered { verdict, by };
        let mut outs =
            self.history.permission(&PermissionEvent::Settled { ask: number, outcome: settled });
        outs.extend(self.status_now(WallMs::now()));
        Some(Answered { outs })
    }

    /// A line Claude Code wrote, heard at `now`. A line that is no JSON is passed over.
    pub fn line(&mut self, line: &[u8], now: WallMs) -> Vec<Out> {
        // A torn or foreign line says nothing of the session.
        let Ok(message) = serde_json::from_slice::<Value>(line) else { return Vec::new() };
        self.incoming(&message, now)
    }

    /// A message Claude Code wrote, heard at `now`.
    pub fn incoming(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        self.heard = now;
        let mut outs = Vec::new();
        if self.stalled {
            self.stalled = false;
            outs.extend(self.history.stalled(None));
        }
        let kind = str_at(message, "type").unwrap_or_default();
        outs.extend(match kind {
            "control_response" => self.response(message, now),
            "control_request" => self.request(message, now),
            "control_cancel_request" => self.cancelled(message, now),
            "command_lifecycle" => self.lifecycle(message, now),
            "system" => self.system(message, now),
            "user" | "assistant" | "stream_event" => {
                // The turn streams again: a request tried again went through.
                let mut outs = if kind != "user"
                    && message.get("parent_tool_use_id").is_none_or(Value::is_null)
                {
                    self.history.retried()
                } else {
                    Vec::new()
                };
                outs.extend(if kind == "stream_event" {
                    self.stream(message, now)
                } else {
                    self.record(message, now)
                });
                outs
            }
            "rate_limit_event" => self.rate_limit(message, now),
            "result" => self.result(message, now),
            _ => Vec::new(),
        });
        outs
    }

    fn response(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(response) = message.get("response") else { return Vec::new() };
        let Some(id) = str_at(response, "request_id") else { return Vec::new() };
        let Some(control) = self.controls.remove(id) else { return Vec::new() };
        let ok = str_at(response, "subtype") == Some("success");
        let said = response.get("response").cloned().unwrap_or(Value::Null);
        if !ok {
            let error = str_at(response, "error").unwrap_or("no reason given").to_owned();
            return self.refused(&control, &error, now);
        }
        match control {
            Control::Initialize => self.initialized(response, &said, now),
            Control::Interrupt => {
                self.interrupting = None;
                Vec::new()
            }
            Control::SetModel(model) => self.meters(|m| {
                m.model = Some(super::model_label(&model));
                m.model_id = Some(model);
            }),
            Control::SetMode(mode) => self.meters(|m| m.mode = Some(mode)),
            Control::Effort(effort) => self.meters(|m| m.effort = Some(effort)),
            Control::Fast(on) => self.history.fact(FAST_FACT, Some(if on { "on" } else { "off" })),
            Control::StopTask => Vec::new(),
            Control::ContextUsage => {
                let used = u64_at(&said, "totalTokens");
                let window = u64_at(&said, "maxTokens");
                self.meters(|m| {
                    m.context_tokens = used.or(m.context_tokens);
                    m.context_window = window.or(m.context_window);
                })
            }
        }
    }

    /// Claude Code refused `control`, saying `error`.
    fn refused(&mut self, control: &Control, error: &str, now: WallMs) -> Vec<Out> {
        let what = match control {
            Control::Initialize => "to start",
            Control::Interrupt => "to stop",
            Control::SetModel(_) => "the model",
            Control::SetMode(_) => "the permission mode",
            Control::Effort(_) => "the effort",
            Control::Fast(_) => "fast mode",
            Control::StopTask => "to stop the subagent",
            Control::ContextUsage => return Vec::new(),
        };
        if *control == Control::Interrupt {
            self.interrupting = None;
        }
        self.history.notice(Notice::INFO, &format!("Claude Code refused {what}: {error}"), now)
    }

    fn meters(&mut self, change: impl FnOnce(&mut Meters)) -> Vec<Out> {
        let mut meters = self.history.meters_now().clone();
        change(&mut meters);
        self.history.meters(meters)
    }

    /// What `initialize` said: the models, the commands, the mode, and what Claude Code still
    /// asks or runs.
    fn initialized(&mut self, response: &Value, said: &Value, now: WallMs) -> Vec<Out> {
        let mut outs = Vec::new();
        if let Some(version) = str_at(said, "claude_code_version") {
            version.clone_into(&mut self.version);
            outs.extend(self.history.version(version));
        }
        let models: Vec<&Value> = said
            .get("models")
            .and_then(Value::as_array)
            .map(|m| m.iter().collect())
            .unwrap_or_default();
        if !models.is_empty() {
            self.fast_models = models
                .iter()
                .filter(|m| bool_at(m, "supportsFastMode"))
                .filter_map(|m| str_at(m, "value").map(str::to_owned))
                .collect();
            let offered = models
                .iter()
                .filter_map(|m| {
                    let id = str_at(m, "value")?.to_owned();
                    let label = str_at(m, "displayName")
                        .map_or_else(|| super::model_label(&id), str::to_owned);
                    let options = if bool_at(m, "supportsFastMode") {
                        vec![fast_offer()]
                    } else {
                        Vec::new()
                    };
                    Some(Model { id, label, options })
                })
                .collect();
            outs.extend(self.history.models(offered));
            let efforts: Vec<Effort> = models
                .iter()
                .find(|m| str_at(m, "value") == Some("default"))
                .or_else(|| models.first())
                .and_then(|m| m.get("supportedEffortLevels"))
                .and_then(Value::as_array)
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|id| Effort {
                            id: id.to_owned(),
                            label: crate::driven::effort_label(id),
                            description: None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let modes: Vec<Mode> = super::offers::offers().modes;
            outs.extend(self.history.offers(modes, efforts));
        }
        if let Some(commands) = said.get("commands").and_then(Value::as_array) {
            let listed = commands
                .iter()
                .filter_map(|c| serde_json::from_value::<crate::commands::Listed>(c.clone()).ok())
                .collect();
            outs.extend(self.history.listed(listed));
        }
        if let Some(mode) = str_at(said, "current_permission_mode") {
            let mode = mode.to_owned();
            outs.extend(self.meters(|m| m.mode = Some(mode)));
        }
        if let Some(fast) = str_at(said, "fast_mode_state") {
            let on = if fast == "on" { "on" } else { "off" };
            outs.extend(self.history.fact(FAST_FACT, Some(on)));
        }
        // What it still asks: each again, those this side knew of already kept.
        let pending = response
            .get("pending_permission_requests")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for asked in pending {
            outs.extend(self.request(&asked, now));
        }
        match str_at(said, "session_state") {
            Some("running" | "requires_action") => {
                self.running = true;
                outs.extend(self.history.working(now));
            }
            Some("idle") if self.running => {
                self.running = false;
                outs.extend(self.history.rest(now));
            }
            _ => {}
        }
        outs.extend(self.status_now(now));
        outs
    }

    /// A request Claude Code makes of Slopty.
    fn request(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(id) = str_at(message, "request_id").map(str::to_owned) else { return Vec::new() };
        let request = message.get("request").cloned().unwrap_or(Value::Null);
        if str_at(&request, "subtype") != Some("can_use_tool") {
            // Nothing else is answered for the person: Claude Code is told this host takes none.
            let subtype = str_at(&request, "subtype").unwrap_or("this").to_owned();
            let line = json!({
                "type": "control_response",
                "response": {
                    "subtype": "error",
                    "request_id": id,
                    "error": format!("Slopty does not answer {subtype} requests"),
                },
            });
            self.writes.push(line.to_string());
            return Vec::new();
        }
        if self.asks.values().any(|a| a.request == id) {
            return Vec::new();
        }
        self.next_ask = self.next_ask.saturating_add(1);
        let number = self.next_ask;
        let tool = str_at(&request, "tool_name").unwrap_or_default().to_owned();
        let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
        let suggestions = request.get("permission_suggestions").cloned().unwrap_or(Value::Null);
        let prompt = PermissionPrompt {
            ask: number,
            call: str_at(&request, "tool_use_id").map(str::to_owned),
            detail: conv::proposed(&tool, &input),
            tool: tool.clone(),
            suggestions: crate::permission::suggestions(&suggestions),
            mode: self.history.meters_now().mode.clone(),
            asked_ms: now,
        };
        self.asks.insert(number, Asked { request: id, tool, input, suggestions });
        let mut outs = self.history.permission(&PermissionEvent::Asked(Box::new(prompt)));
        outs.extend(self.status_now(now));
        outs
    }

    fn cancelled(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(id) = str_at(message, "request_id") else { return Vec::new() };
        let Some((&number, _)) = self.asks.iter().find(|(_, a)| a.request == id) else {
            return Vec::new();
        };
        self.asks.remove(&number);
        let mut outs = self
            .history
            .permission(&PermissionEvent::Settled { ask: number, outcome: Settled::Withdrawn });
        outs.extend(self.status_now(now));
        outs
    }

    fn lifecycle(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(uuid) = str_at(message, "command_uuid") else { return Vec::new() };
        let mut outs = Vec::new();
        let state = str_at(message, "state");
        if matches!(state, Some("queued" | "started")) {
            if self.acknowledged(uuid).is_some() {
                outs.extend(self.shown());
            }
            if state == Some("started") && self.last_echo.as_deref() != Some(uuid) {
                self.awaiting_echo = Some(uuid.to_owned());
            }
            if state == Some("started") && !self.running {
                self.running = true;
                outs.extend(self.history.working(now));
            }
        }
        outs
    }

    fn system(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        match str_at(message, "subtype").unwrap_or_default() {
            "init" => {
                let mut outs = Vec::new();
                if let Some(model) = str_at(message, "model").map(str::to_owned) {
                    outs.extend(self.meters(|m| {
                        m.model = Some(super::model_label(&model));
                        m.model_id = Some(model);
                    }));
                }
                if let Some(mode) = str_at(message, "permissionMode").map(str::to_owned) {
                    outs.extend(self.meters(|m| m.mode = Some(mode)));
                }
                if let Some(version) = str_at(message, "claude_code_version") {
                    outs.extend(self.history.version(version));
                }
                outs
            }
            "status" => {
                let mut outs = Vec::new();
                if let Some(mode) = str_at(message, "permissionMode").map(str::to_owned) {
                    outs.extend(self.meters(|m| m.mode = Some(mode)));
                }
                let compacting = str_at(message, "status") == Some("compacting");
                outs.extend(self.history.compacting(compacting));
                if !self.running && str_at(message, "status") == Some("requesting") {
                    self.running = true;
                    outs.extend(self.history.working(now));
                }
                outs
            }
            "session_state_changed" => match str_at(message, "state") {
                Some("running") if !self.running => {
                    self.running = true;
                    self.history.working(now)
                }
                _ => Vec::new(),
            },
            "api_retry" => {
                let count = |name: &str| u64_at(message, name).and_then(|n| u32::try_from(n).ok());
                let retry = slopty_proto::thread::Retry {
                    attempt: count("attempt").unwrap_or(1),
                    max: count("max_retries"),
                    in_ms: u64_at(message, "retry_delay_ms"),
                };
                let error = message.get("error");
                let said = error
                    .and_then(|e| str_at(e, "message").or_else(|| e.as_str()))
                    .unwrap_or("The API failed");
                // Its kind, as the API names it, else the HTTP status it failed with.
                let kind = error
                    .and_then(|e| str_at(e, "type"))
                    .map(str::to_owned)
                    .or_else(|| u64_at(message, "error_status").map(|status| status.to_string()));
                let text = kind.map_or_else(|| said.to_owned(), |kind| format!("{kind}: {said}"));
                self.history.retrying(&text, retry, now)
            }
            "task_started" => {
                if let (Some(task), Some(call)) =
                    (str_at(message, "task_id"), str_at(message, "tool_use_id"))
                {
                    self.agents.insert(call.to_owned(), task.to_owned());
                }
                Vec::new()
            }
            "task_notification" => {
                // As the transcript records it, so the decoder settles the subagent's run.
                let field = |name: &str| str_at(message, name).unwrap_or_default();
                let content = format!(
                    "<task-notification>\n<task-id>{}</task-id>\n<tool-use-id>{}</tool-use-id>\n\
                     <output-file>{}</output-file>\n<status>{}</status>\n<summary>{}</summary>\n\
                     </task-notification>",
                    field("task_id"),
                    field("tool_use_id"),
                    field("output_file"),
                    field("status"),
                    field("summary"),
                );
                let record = json!({
                    "type": "queue-operation",
                    "operation": "enqueue",
                    "timestamp": stamp(now),
                    "content": content,
                });
                let changes = self.decoder.ingest(&conv::ThreadId::Main, &record);
                self.history.transcript(&changes, &[])
            }
            "background_tasks_changed" => {
                let tasks = message.get("tasks").and_then(Value::as_array);
                let count = tasks.map_or(0, Vec::len);
                let named = tasks
                    .and_then(|t| t.first())
                    .and_then(|t| str_at(t, "description"))
                    .map(str::to_owned);
                self.history.background(u32::try_from(count).unwrap_or(u32::MAX), 0, named)
            }
            "compact_boundary" => {
                let meta = message.get("compact_metadata");
                let record = json!({
                    "type": "system",
                    "subtype": "compact_boundary",
                    "uuid": str_at(message, "uuid").unwrap_or_default(),
                    "timestamp": stamp(now),
                    "compactMetadata": {
                        "trigger": meta.and_then(|m| str_at(m, "trigger")),
                        "preTokens": meta.and_then(|m| u64_at(m, "pre_tokens")),
                        "postTokens": meta.and_then(|m| u64_at(m, "post_tokens")),
                    },
                });
                self.compacted = true;
                let changes = self.decoder.ingest(&conv::ThreadId::Main, &record);
                self.history.transcript(&changes, &[])
            }
            "informational" => {
                let mut record = message.clone();
                stamped(&mut record, now);
                let changes = self.decoder.ingest(&conv::ThreadId::Main, &record);
                self.history.transcript(&changes, &[])
            }
            "session_title_changed" => {
                str_at(message, "title").map(|t| self.history.named(t)).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// A user or assistant message: a record of the conversation, its subagent's when it names
    /// the call that started one.
    ///
    /// The stream tells a slash command's output before the echo of the command, which the
    /// transcript records first: what a command printed waits for the command it answers.
    fn record(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let uuid = str_at(message, "uuid");
        if let Some(awaited) = self.awaiting_echo.as_deref()
            && uuid != Some(awaited)
            && str_at(message, "type") == Some("user")
            && text_of(message).starts_with(COMMAND_STDOUT)
        {
            self.held.push(message.clone());
            return Vec::new();
        }
        let echoed = uuid.is_some() && uuid == self.awaiting_echo.as_deref();
        if str_at(message, "type") == Some("user") {
            self.last_echo = uuid.map(str::to_owned);
        }
        let mut outs = self.one_record(message, now);
        if echoed {
            self.awaiting_echo = None;
            outs.extend(self.release_held(now));
        }
        outs
    }

    /// What waited for an echo goes as it came.
    fn release_held(&mut self, now: WallMs) -> Vec<Out> {
        let mut outs = Vec::new();
        for held in std::mem::take(&mut self.held) {
            outs.extend(self.one_record(&held, now));
        }
        outs
    }

    fn one_record(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let mut outs = Vec::new();
        let mut record = message.clone();
        stamped(&mut record, now);
        let parent = str_at(message, "parent_tool_use_id").map(str::to_owned);
        let agent = str_at(message, "agent_id")
            .map(str::to_owned)
            .or_else(|| parent.as_ref().and_then(|call| self.agents.get(call).cloned()));
        if let Some(fields) = record.as_object_mut() {
            // What the stream names in its own words, as the transcript names it.
            for (streamed, recorded) in
                [("tool_use_result", "toolUseResult"), ("session_id", "sessionId")]
            {
                if !fields.contains_key(recorded)
                    && let Some(value) = fields.get(streamed).cloned()
                {
                    fields.insert(recorded.to_owned(), value);
                }
            }
            if let (Some(_), Some(agent)) = (&parent, &agent) {
                fields.insert("isSidechain".to_owned(), Value::Bool(true));
                fields.insert("agentId".to_owned(), Value::String(agent.clone()));
            }
            // A message Claude Code wrote itself: the summary right after a compaction, else
            // none of the person's.
            if fields.get("isSynthetic").and_then(Value::as_bool) == Some(true) {
                let flag =
                    if std::mem::take(&mut self.compacted) { "isCompactSummary" } else { "isMeta" };
                fields.insert(flag.to_owned(), Value::Bool(true));
            }
        }
        // The echo of a message Slopty gave, or its first word of one it queued.
        if let Some(uuid) = str_at(message, "uuid")
            && self.acknowledged(uuid).is_some()
        {
            outs.extend(self.shown());
        }
        if str_at(message, "type") == Some("assistant") {
            self.settle(&record);
        }
        let changes = self.decoder.ingest(&conv::ThreadId::Main, &record);
        outs.extend(self.history.transcript(&changes, &[]));
        outs
    }

    /// The live block an assistant message settles: the oldest of its message not settled yet.
    fn settle(&mut self, record: &Value) {
        let Some(message) = record.get("message") else { return };
        let Some(id) = str_at(message, "id") else { return };
        let Some(uuid) = str_at(record, "uuid") else { return };
        let Some(blocks) = self.blocks.get_mut(id) else { return };
        let Some((live, _, settled)) = blocks.iter_mut().find(|(_, _, settled)| !*settled) else {
            return;
        };
        *settled = true;
        let entry = format!("{uuid}:0");
        let live = live.clone();
        let tool = message
            .get("content")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|b| str_at(b, "id"))
            .map(str::to_owned);
        self.history.settle(&live, tool.as_deref().unwrap_or(&entry));
    }

    /// A piece of what the model is writing, streamed.
    fn stream(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(event) = message.get("event") else { return Vec::new() };
        let parent = str_at(message, "parent_tool_use_id").map(str::to_owned);
        let thread = match parent.as_ref().and_then(|call| self.agents.get(call)) {
            Some(agent) => conv::ThreadId::Agent(agent.clone()),
            None if parent.is_some() => return Vec::new(),
            None => conv::ThreadId::Main,
        };
        match str_at(event, "type").unwrap_or_default() {
            "message_start" => {
                if let Some(id) = event.get("message").and_then(|m| str_at(m, "id")) {
                    self.streaming.insert(parent, id.to_owned());
                }
                Vec::new()
            }
            "content_block_start" => {
                let Some(message_id) = self.streaming.get(&parent).cloned() else {
                    return Vec::new();
                };
                let index = u64_at(event, "index").unwrap_or_default();
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                let kind = match str_at(&block, "type") {
                    Some("text") => LiveKind::Text,
                    Some("thinking") => LiveKind::Thinking,
                    Some("tool_use") => LiveKind::Tool {
                        id: str_at(&block, "id").unwrap_or_default().to_owned(),
                        name: str_at(&block, "name").unwrap_or_default().to_owned(),
                    },
                    _ => return Vec::new(),
                };
                let id = LiveId {
                    turn: message_id.clone(),
                    step: 0,
                    block: u32::try_from(index).unwrap_or(u32::MAX),
                };
                self.blocks.entry(message_id).or_default().push((
                    id.clone(),
                    thread.clone(),
                    false,
                ));
                self.history.live(Live::Start { thread, id, kind }, now)
            }
            "content_block_delta" => {
                let Some(message_id) = self.streaming.get(&parent).cloned() else {
                    return Vec::new();
                };
                let index = u64_at(event, "index").unwrap_or_default();
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                let text = match str_at(&delta, "type") {
                    Some("text_delta") => str_at(&delta, "text"),
                    Some("thinking_delta") => str_at(&delta, "thinking"),
                    Some("input_json_delta") => str_at(&delta, "partial_json"),
                    _ => None,
                };
                let Some(text) = text.filter(|t| !t.is_empty()) else { return Vec::new() };
                let id = LiveId {
                    turn: message_id,
                    step: 0,
                    block: u32::try_from(index).unwrap_or(u32::MAX),
                };
                self.history.live(Live::Append { id, text: text.to_owned() }, now)
            }
            _ => Vec::new(),
        }
    }

    fn rate_limit(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(info) = message.get("rate_limit_info") else { return Vec::new() };
        let resets = u64_at(info, "resetsAt").map(|s| WallMs::from_millis(s.saturating_mul(1000)));
        let name = match str_at(info, "rateLimitType") {
            Some("five_hour") => "five-hour",
            Some("seven_day" | "seven_day_opus" | "seven_day_sonnet") => "seven-day",
            Some(other) => other,
            None => "limit",
        }
        .to_owned();
        let used = info.get("utilization").and_then(Value::as_f64);
        let rejected = str_at(info, "status") == Some("rejected");
        let used_bp = if rejected { 10_000 } else { used.map_or(0, basis_points) };
        let mut outs = self.meters(|m| {
            m.limits.retain(|l| l.name != name);
            m.limits.push(slopty_proto::thread::Limit { name, used_bp, resets_ms: resets });
        });
        if rejected {
            self.limit_until = resets.or(Some(now));
            outs.extend(self.history.limited(self.limit_until));
        }
        outs
    }

    fn result(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        // Whatever waited for an echo that never came goes before the turn ends.
        self.awaiting_echo = None;
        let mut outs = self.release_held(now);
        let was_running = self.running;
        self.running = false;
        self.interrupting = None;
        outs.extend(self.history.stopping(None));
        outs.extend(self.history.compacting(false));
        let failed = bool_at(message, "is_error");
        let interrupted = str_at(message, "subtype") == Some("error_during_execution")
            || str_at(message, "terminal_reason").is_some_and(|r| r.starts_with("aborted"));
        let end = if interrupted {
            End::Interrupted
        } else if failed {
            End::Failed(str_at(message, "result").map(str::to_owned).filter(|r| !r.is_empty()))
        } else {
            End::Complete
        };
        if was_running || str_at(message, "local_command").is_none() {
            outs.extend(self.history.ended(&end, now));
        }
        if let Some(model_usage) = message.get("modelUsage").and_then(Value::as_object) {
            let window = model_usage.values().filter_map(|u| u64_at(u, "contextWindow")).max();
            if window.is_some() {
                outs.extend(self.meters(|m| m.context_window = window));
            }
        }
        self.control(Control::ContextUsage, &json!({ "subtype": "get_context_usage" }));
        outs.extend(self.status_now(now));
        outs
    }

    /// The status as it stands: asking the person while a request is open, at work while a turn
    /// runs, else at rest.
    fn status_now(&mut self, now: WallMs) -> Vec<Out> {
        let status = self.history.phase().cloned();
        let liveness = status.as_ref().map_or(Liveness::Live, |s| s.liveness);
        if liveness != Liveness::Live {
            return Vec::new();
        }
        let (phase, wait) = if let Some(asked) = self.asks.values().next() {
            let kind = if asked.tool == "AskUserQuestion" { "question" } else { "permission" };
            let text = super::history::asks_for(&asked.tool, &asked.input);
            (Phase::NeedsYou, Some(Wait { kind: kind.to_owned(), text }))
        } else if self.running {
            (Phase::Working, None)
        } else {
            match status.as_ref().map(|s| s.phase) {
                Some(phase @ (Phase::Done | Phase::Failed | Phase::Stopped | Phase::Idle)) => {
                    (phase, None)
                }
                _ => (Phase::Idle, None),
            }
        };
        let since = status.as_ref().filter(|s| s.phase == phase).map_or(now, |s| s.since_ms);
        self.history.status(phase, wait, Liveness::Live, since)
    }

    /// When [`Self::tick`] is next due, if anything waits on time.
    #[must_use]
    pub fn deadline(&self) -> Option<WallMs> {
        let after = |at: WallMs, wait: Duration| {
            WallMs::from_millis(at.as_millis().saturating_add(ms(wait)))
        };
        let interrupt = self.interrupting.map(|at| after(at, INTERRUPT_ACK));
        let delivery =
            self.sends.iter().filter(|s| !s.line.is_empty()).map(|s| after(s.at, DELIVERY)).min();
        let limit = self.limit_until.map(|at| after(at, LIMIT_SLACK));
        let stall = self.running.then(|| after(self.looked.unwrap_or(self.heard), TICK));
        [interrupt, delivery, limit, stall].into_iter().flatten().min()
    }

    /// Time has come to `now`. Whether the process is to be ended: a stop went unacknowledged.
    pub fn tick(&mut self, now: WallMs) -> (Vec<Out>, bool) {
        let mut outs = Vec::new();
        let past =
            |at: WallMs, wait: Duration| now.as_millis() >= at.as_millis().saturating_add(ms(wait));
        let escalate = self.interrupting.is_some_and(|at| past(at, INTERRUPT_ACK));
        // Messages not acknowledged: sent again once, then held as maybe not arrived.
        let mut changed = false;
        for sent in &mut self.sends {
            if sent.line.is_empty() || !past(sent.at, DELIVERY) {
                continue;
            }
            if sent.again {
                sent.line.clear();
                sent.pending.state = PendingState::Held { reason: Pending::UNKNOWN.to_owned() };
            } else {
                sent.again = true;
                sent.at = now;
                self.writes.push(sent.line.clone());
            }
            changed = true;
        }
        if changed {
            outs.extend(self.shown());
        }
        if let Some(until) = self.limit_until
            && past(until, LIMIT_SLACK)
        {
            self.limit_until = None;
            outs.extend(self.history.limited(None));
            outs.extend(self.released());
        }
        if self.running {
            let looked = self.looked.unwrap_or(self.heard);
            let woke = now.as_millis().saturating_sub(looked.as_millis()) > ms(WAKE_GAP);
            if woke {
                // The machine slept: the time since is not the turn's.
                self.heard = now;
            } else if past(self.heard, STALL) && !self.stalled {
                self.stalled = true;
                outs.extend(self.history.stalled(Some(STALLED_TEXT.to_owned())));
            }
            self.looked = Some(now);
        } else {
            self.looked = None;
        }
        (outs, escalate)
    }

    /// What the usage limit held, waiting again.
    fn held_for_limit(&mut self) -> Vec<Out> {
        let mut pending: Vec<Pending> = self.sends.iter().map(|s| s.pending.clone()).collect();
        if let slopty_proto::thread::Action::PendingSet(held) = self.queued.shown() {
            pending.extend(held.into_iter().map(|mut p| {
                if p.state == PendingState::Waiting {
                    p.state = PendingState::Held { reason: HELD_FOR_LIMIT.to_owned() };
                }
                p
            }));
        }
        self.history.pending(pending)
    }

    /// The limit lifted: what it held is queued again.
    fn released(&mut self) -> Vec<Out> {
        self.shown()
    }

    /// Claude Code is being ended, at `now`.
    pub fn ending(&mut self, now: WallMs) -> Vec<Out> {
        self.ending = Some(now);
        self.history.stopping(Some(STOPPING_TEXT.to_owned()))
    }

    /// Claude Code is gone, at `now`, for the reason in `why` when it failed: what it asked is
    /// no longer asked, its calls are cancelled, the turn under way ends stopped or failed, and
    /// what was on its way may not have reached it. The thread goes on by its next message,
    /// which starts Claude Code again on the session.
    pub fn exited(&mut self, why: Option<&str>, now: WallMs) -> Vec<Out> {
        let mut outs = Vec::new();
        for (number, _) in std::mem::take(&mut self.asks) {
            outs.extend(self.history.permission(&PermissionEvent::Settled {
                ask: number,
                outcome: Settled::Withdrawn,
            }));
        }
        for sent in &mut self.sends {
            sent.line.clear();
            sent.pending.state = PendingState::Held { reason: Pending::UNKNOWN.to_owned() };
        }
        outs.extend(self.shown());
        if let Some(why) = why {
            outs.extend(self.history.notice(Notice::INFO, why, now));
        }
        if self.running {
            // A stop asked for and not acknowledged ends the process: the turn ended stopped.
            let end = match why {
                Some(why) if self.interrupting.is_none() => End::Failed(Some(why.to_owned())),
                _ => End::Interrupted,
            };
            outs.extend(self.history.ended(&end, now));
        }
        self.running = false;
        self.interrupting = None;
        self.ending = None;
        outs.extend(self.history.stopping(None));
        outs.extend(self.history.gone(now));
        outs
    }
}

/// Fast mode, as a model that has it offers it.
fn fast_offer() -> OptionOffer {
    OptionOffer {
        id: super::FAST.to_owned(),
        label: "Fast mode".to_owned(),
        choices: vec![
            OptionChoice { id: "off".to_owned(), label: "Off".to_owned() },
            OptionChoice { id: "on".to_owned(), label: "On".to_owned() },
        ],
        default: "off".to_owned(),
    }
}

/// A user message's content: the words, and each picture as an image block; any other file by
/// its path in the words.
fn content_of(text: &str, attached: &[Attached]) -> Value {
    let words = crate::attach::with_files(text, attached);
    let mut blocks: Vec<Value> = crate::attach::pictures(attached)
        .filter_map(|picture| match picture {
            Attached::Picture { media_type, .. } => Some(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": picture.base64()? },
            })),
            Attached::File { .. } => None,
        })
        .collect();
    blocks.push(json!({ "type": "text", "text": words }));
    Value::Array(blocks)
}

/// A share (`0.0..=1.0`) in basis points.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=10 000 first"
)]
fn basis_points(share: f64) -> u32 {
    (share * 10_000.0).clamp(0.0, 10_000.0).round() as u32
}

fn ms(wait: Duration) -> u64 {
    u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)
}

/// `now` as Claude Code stamps its records.
fn stamp(now: WallMs) -> String {
    conv::format_ms(now)
}

/// `record` stamped with `now` where it carries no time of its own.
fn stamped(record: &mut Value, now: WallMs) {
    if let Some(fields) = record.as_object_mut()
        && !fields.contains_key("timestamp")
    {
        fields.insert("timestamp".to_owned(), Value::String(stamp(now)));
    }
}

/// The words of a user message's content, its text blocks joined.
fn text_of(message: &Value) -> String {
    match message.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks.iter().filter_map(|b| str_at(b, "text")).collect(),
        _ => String::new(),
    }
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn u64_at(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn bool_at(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests;
