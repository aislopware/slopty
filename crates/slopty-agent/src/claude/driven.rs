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
//! - **Interrupts.** A stop is one ladder that always lands. It goes as a control request; one
//!   Claude Code does not acknowledge in [`INTERRUPT_ACK`], or refuses, is escalated: the worker
//!   ends the process, the turn is cut, and the next message takes the session up again. One it
//!   acknowledges and whose turn does not end within [`END_WAIT`] is escalated the same way.
//! - **Starting.** An `initialize` Claude Code does not answer in [`INIT_WAIT`] is said ("Claude
//!   Code did not start") and the process is ended, as a stop is.
//! - **Approvals.** Claude Code asks about each tool call it may not run alone
//!   (`--permission-prompt-tool=stdio`): a request on the call's item, answered with the decision
//!   it takes, by the request's own id. An `initialize` answer names those still open, so a worker
//!   that comes back to a running Claude Code asks them again.
//! - **Waits.** Compacting, the plan's usage limit (messages are held until it resets, then go), a
//!   stop under way, and a turn that shows no progress for [`STALL`] are said as waits; a gap
//!   between ticks longer than [`WAKE_GAP`] is a machine waking, never a stall.
//! - **The work beside the conversation** (`work`): every background task Claude Code says it
//!   started, how it goes and how it ended, each call's timer, the spinner's figures, its
//!   notifications, the goal, the next prompt it suggests, and the rows its TUI derives (allowed
//!   commands, saved memories, a fallback model, hooks that failed).
//! - **Its own asks** (`asks`): every dialog kind it has and every MCP form, each a request of the
//!   thread answered by the person, never for them.
//! - **Its doors** (`doors`): an MCP App's resource read and tool called for its page, the
//!   permission rules, the context's categories, a call moved to the background, files put back, a
//!   `!` command's output handed over, and the surfaces its mods draw.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::Duration;

use serde_json::{Value, json};
use slopty_core::WallMs;
use slopty_proto::thread::{
    Answerer, AskId, Cap, Delivery, Effort, IntentId, Liveness, Meters, Mode, Model, Notice,
    OptionChoice, OptionOffer, Pending, PendingState, Phase, ThreadMeta, ThreadState, Wait,
};

use super::history::{End, History, Out};
use crate::attach::Attached;
use crate::conversation::output::Outputs;
use crate::conversation::{
    self as conv, Conversation, Live, LiveId, LiveKind, PermissionEvent, PermissionPrompt, Settled,
    Verdict,
};
use crate::queue::Queue;

mod asks;
mod doors;
mod ending;
mod model;
mod work;

pub use asks::DIALOG_KINDS;
pub use doors::{Ran, RewindPreview, SURFACE_CLIENT};
pub use ending::{ENDED, Ending, Kind as EndKind, ending};
pub use model::THINKING_FACT;
use model::{mode_words, switched_to};
pub use work::{PROGRESS_EVERY, TAIL_EVERY};

/// What a driven Claude Code can do through Slopty.
pub const CAPS: [&str; 21] = [
    Cap::APPROVALS,
    Cap::BACKGROUND,
    Cap::END,
    Cap::FORK,
    Cap::INTERRUPT,
    Cap::MCP_SERVERS,
    Cap::QUEUE,
    Cap::REWIND,
    Cap::REWIND_CONVERSATION,
    Cap::RULES,
    Cap::SCHEDULE,
    Cap::SET_EFFORT,
    Cap::SET_MODE,
    Cap::SET_MODEL,
    Cap::SET_OPTION,
    Cap::SHELL,
    Cap::SIDE,
    Cap::SNAPSHOTS,
    Cap::STEER,
    Cap::STOP_TASK,
    Cap::SURFACES,
];

/// How long a message may go unacknowledged before it is sent again, and then before it is held
/// as one that may not have arrived.
pub const DELIVERY: Duration = Duration::from_secs(120);

/// How long a stop may go unacknowledged before the worker ends the process.
pub const INTERRUPT_ACK: Duration = Duration::from_millis(1500);

/// How long a turn has to end once Claude Code acknowledged its stop, before the worker ends the
/// process (Orca's request end wait, zeron's stop settle).
pub const END_WAIT: Duration = Duration::from_secs(3);

/// How long Claude Code has to answer `initialize` before it is said not to have started.
pub const INIT_WAIT: Duration = Duration::from_secs(30);

/// What a Claude Code that never answered `initialize` is said with.
pub const DID_NOT_START: &str = "Claude Code did not start";

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

/// The fact that keeps the thread's fast mode as Claude Code says it stands ([`super::FAST`]):
/// `on`, `off`, or `cooldown` while a fast limit holds it back.
pub const FAST_FACT: &str = "claude-fast";

/// The fact that says whether ultracode (Claude Code's standing workflow orchestration) is on for
/// the thread: `on` or `off`, and absent where the session cannot have it.
pub const ULTRACODE_FACT: &str = "claude-ultracode";

/// The option a model whose thinking can be turned off offers ([`Model::options`]).
pub const THINKING: &str = "thinking";

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
    /// Thinking turned on or off (`set_max_thinking_tokens`).
    Thinking(bool),
    /// The settings as they apply, for ultracode (`get_settings`).
    Settings,
    StopTask,
    ContextUsage,
    /// A call, or every blocking one, moved to the background.
    Background(Option<String>),
    /// The permission rules listed.
    Rules,
    /// The MCP servers and their tools, for the apps among them.
    McpStatus,
    /// An MCP server connected again, on the person's word.
    McpReconnect(String),
    /// An MCP server turned on or off, on the person's word.
    McpEnable(String, bool),
    /// A page's ask of its app, by its number.
    App(u64),
    /// The files put back to before turn `turn`, with the later turns' messages to ask by
    /// when this one has no checkpoint.
    Rewind(slopty_proto::thread::TurnId, Vec<String>),
    /// The files a rewind would put back, asked without putting them back, by the ask's number,
    /// with the later messages to ask by as [`Control::Rewind`] has them.
    RewindPreview(u64, Vec<String>),
    /// The conversation taken back to before turn `turn` (`rewind_conversation`).
    RewindConversation(slopty_proto::thread::TurnId),
    /// One of the asks a mod's surfaces make.
    Ui(doors::Ui),
    /// A side question, asked as it stands until answered.
    Side(slopty_proto::thread::SideExchange),
    /// The person's name for the thread, given the session as theirs (`rename_session`).
    Rename(String),
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

/// A permission, a dialog or a form Claude Code asks for, open.
#[derive(Clone, Debug)]
struct Asked {
    /// The control request's id, which the answer names.
    request: String,
    tool: String,
    input: Value,
    suggestions: Value,
    /// What kind of ask it is.
    form: asks::Form,
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
    /// A stop was acknowledged while its turn ran, since then: the turn has [`END_WAIT`] to end.
    stop_acked: Option<WallMs>,
    /// `initialize` was asked and not answered, since then.
    starting: Option<WallMs>,
    /// How long `initialize` has to be answered ([`INIT_WAIT`] unless set).
    init_wait: Duration,
    /// The last tool call the model made, in a few words, for a stall to name.
    last_call: Option<String>,
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
    /// The conversation was cleared (`conversation_reset`): the session it goes on in is the
    /// next one a message names.
    clearing: bool,
    /// The last user message heard.
    last_echo: Option<String>,
    /// A compaction was said, and its summary is still to come.
    compacted: bool,
    /// Each call's tool and input, by the call, as the model wrote it.
    calls: HashMap<String, (String, Value)>,
    /// When each running call was last noted ([`work::PROGRESS_EVERY`]).
    noted: HashMap<String, WallMs>,
    /// The spinner's figures as last told, and when.
    activity: work::Spin,
    /// The goal as last told.
    goal: Option<slopty_proto::thread::Goal>,
    /// The tails of the background commands, read for their output ([`work::TAIL_EVERY`]).
    outputs: Outputs,
    tailed: Option<WallMs>,
    /// The answers to pages' asks of their apps, not taken yet.
    app_answers: Vec<(u64, slopty_proto::visual::AppAnswer)>,
    /// What rewinds would put back, by each ask's number, not taken yet.
    rewind_previews: Vec<(u64, RewindPreview)>,
    /// A rewind of the conversation that waits for the files to go back first: Claude Code
    /// finds a message's checkpoint only while the message is in the conversation.
    held_rewind: Option<slopty_proto::thread::TurnId>,
    /// The MCP servers' statuses as last seen, so a change asks for their tools again.
    servers: String,
    /// The surfaces the session's mods draw, and the elements in them.
    surfaces: doors::Surfaces,
    /// The control requests whose refusal goes unsaid (a surface Claude Code does not have).
    quiet: HashSet<String>,
    /// The mode plan mode was entered from, which a plan approved with a clear context keeps.
    before_plan: Option<String>,
    /// The session began in bypass permissions, where plan mode asks before almost nothing.
    started_bypass: bool,
    /// The notes said once a session (a usage limit's warning, a settings file's error), and
    /// whether usage credits are drawn on.
    said_once: (HashSet<String>, bool),
    /// The person asked for the goal to be cleared: its end is no achievement.
    goal_clearing: bool,
    /// Fast mode as Claude Code last said it stands, and whether the person turned it on.
    fast: (Option<String>, bool),
    /// A turn of this process has ended: the next one reads its context from the prompt cache,
    /// unless the cache expired while it waited.
    turned: bool,
    /// The person's own name for the thread in Slopty (`ThreadState::named`), which Claude
    /// Code's session takes as theirs; empty for none.
    person_named: String,
    /// The name Claude Code's session holds as the person's (`custom-title`), as last read or
    /// given it.
    custom_title: Option<String>,
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
        state.named.clone_into(&mut driven.person_named);
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
            stop_acked: None,
            starting: None,
            init_wait: INIT_WAIT,
            last_call: None,
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
            clearing: false,
            last_echo: None,
            compacted: false,
            calls: HashMap::new(),
            noted: HashMap::new(),
            activity: work::Spin::default(),
            goal: None,
            outputs: Outputs::default(),
            tailed: None,
            app_answers: Vec::new(),
            rewind_previews: Vec::new(),
            held_rewind: None,
            servers: String::new(),
            surfaces: doors::Surfaces::default(),
            quiet: HashSet::new(),
            before_plan: None,
            started_bypass: false,
            said_once: (HashSet::new(), false),
            goal_clearing: false,
            fast: (None, false),
            turned: false,
            person_named: String::new(),
            custom_title: None,
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
        self.running || self.sends.iter().any(|s| s.pending.state == PendingState::Sending)
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
        // A session read again takes Claude Code's own title for it: the name given it in
        // Claude Code, else the title Claude Code made. The person's name from Slopty, which
        // the session holds too, is theirs and stays on the thread's own mark.
        let (custom, ai) = super::history::titles_in(records);
        self.custom_title = custom.map(str::to_owned);
        let theirs = custom.filter(|c| *c != self.person_named).or(ai);
        if let Some(title) = theirs {
            outs.extend(self.history.named(title));
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

    /// Give `initialize` `wait` to be answered instead of [`INIT_WAIT`]: a worker whose tests
    /// prove the start that never comes waits on their scale.
    pub const fn set_init_wait(&mut self, wait: Duration) {
        self.init_wait = wait;
    }

    /// The `initialize` request, which goes first to every Claude Code this drives, and again to
    /// one the worker comes back to.
    ///
    /// It declares what Slopty shows: every dialog kind ([`DIALOG_KINDS`]), the tool calls of
    /// subagents other subagents start, each subagent's progress summary, and a stop for each
    /// background task.
    pub fn initialize(&mut self) {
        let request = json!({
            "subtype": "initialize",
            "supportedDialogKinds": DIALOG_KINDS,
            "forwardNestedSubagentToolCalls": true,
            "agentProgressSummaries": true,
            "perTaskStopAffordance": true,
        });
        self.control(Control::Initialize, &request);
        self.starting = Some(self.heard);
    }

    fn control(&mut self, control: Control, request: &Value) -> String {
        self.next = self.next.saturating_add(1);
        let id = format!("slopty-{}", self.next);
        let line = json!({ "type": "control_request", "request_id": id, "request": request });
        self.controls.insert(id.clone(), control);
        self.writes.push(line.to_string());
        id
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
        // Claude Code says a goal cleared this way itself ("Goal cleared: …"): its end is no
        // achievement.
        if text.trim() == "/goal clear" {
            self.goal_clearing = true;
        }
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

    /// The person named the thread `title` in Slopty: Claude Code's session takes it as the
    /// person's own (`rename_session` from the host), so its own list of sessions agrees. A
    /// name the session holds already goes nowhere, and an empty one stays in Slopty, since
    /// Claude Code takes no empty name.
    pub fn rename(&mut self, title: &str) {
        let title = title.trim();
        title.clone_into(&mut self.person_named);
        if title.is_empty() || self.custom_title.as_deref() == Some(title) {
            return;
        }
        let request = json!({
            "subtype": "rename_session", "title": title, "source": "host",
            "session_id": self.history.meta().native,
        });
        let id = self.control(Control::Rename(title.to_owned()), &request);
        self.quiet.insert(id);
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
        self.give_as(uuid, pending, content, true, now)
    }

    /// Give `content` as the person's message `uuid`: one Claude Code answers when `query`, else
    /// one it only takes into the conversation, answered with the next that asks.
    fn give_as(
        &mut self,
        uuid: String,
        pending: Pending,
        content: &Value,
        query: bool,
        now: WallMs,
    ) -> Vec<Out> {
        let mut message = json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "parent_tool_use_id": null,
            "session_id": self.history.meta().native,
            "uuid": uuid,
        });
        if !query && let Some(fields) = message.as_object_mut() {
            fields.insert("shouldQuery".to_owned(), Value::Bool(false));
        }
        let line = message.to_string();
        self.writes.push(line.clone());
        self.sends.push_back(Sent { uuid, pending, line, at: now, again: false });
        self.stopped = false;
        // What it suggested goes once the person says anything.
        let mut outs = self.unsuggest();
        outs.extend(self.shown());
        outs
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
        if !matches!(choice, "on" | "off") {
            return false;
        }
        let on = choice == "on";
        match option {
            super::FAST => {
                self.fast.1 = on;
                let request =
                    json!({ "subtype": "apply_flag_settings", "settings": { "fastMode": on } });
                self.control(Control::Fast(on), &request);
            }
            // Off is no thinking budget at all; on gives the model its own again.
            THINKING => {
                let budget = if on { Value::Null } else { json!(0) };
                let request =
                    json!({ "subtype": "set_max_thinking_tokens", "max_thinking_tokens": budget });
                self.control(Control::Thinking(on), &request);
            }
            _ => return false,
        }
        true
    }

    /// Whether request `ask` is open and takes `choice`.
    #[must_use]
    pub fn takes(&self, ask: &AskId, choice: &str) -> bool {
        let Some(asked) = ask.0.parse::<u64>().ok().and_then(|n| self.asks.get(&n)) else {
            return false;
        };
        if !matches!(asked.form, asks::Form::Tool) {
            return asks::result_of(&asked.form, choice).is_some();
        }
        if super::history::clear_mode(choice).is_some() {
            return asked.tool == "ExitPlanMode";
        }
        if choice == super::history::CLARIFY {
            return asked.tool == "AskUserQuestion";
        }
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
        now: WallMs,
    ) -> Option<Answered> {
        if !self.takes(ask, choice) {
            return None;
        }
        let number = ask.0.parse::<u64>().ok()?;
        let asked = self.asks.remove(&number)?;
        if !matches!(asked.form, asks::Form::Tool) {
            return Some(self.answer_ask(ask, &asked, choice, by));
        }
        if super::history::clear_mode(choice).is_some() {
            return Some(self.plan_cleared(ask, &asked, choice, by, now));
        }
        let verdict = if choice == super::history::CLARIFY {
            Verdict::Deny { message: clarify(&asked.input), interrupt: false }
        } else {
            super::history::verdict(choice, message)?
        };
        let words = matches!(verdict, Verdict::Allow | Verdict::AllowAlways)
            .then(|| message.map(str::trim).filter(|m| !m.is_empty()).map(str::to_owned))
            .flatten();
        // A plan approved with words is approved as the person edited it, as the TUI's Ctrl+G
        // does: the plan goes back in the call's input, and Claude Code saves it to the plan
        // file and echoes it to the model.
        let edited = (asked.tool == "ExitPlanMode").then(|| words.clone()).flatten();
        let mut input = asked.input.clone();
        if let (Some(plan), Some(fields)) = (&edited, input.as_object_mut()) {
            fields.insert("plan".to_owned(), Value::String(plan.clone()));
        }
        let mut decision = crate::permission::decision(&verdict, &input, &asked.suggestions);
        // Answers to the agent's questions carry what the TUI's dialog adds to them: the preview
        // of each option picked, and the person's notes on it (the words beside the answers).
        if let (Verdict::Answer { answers }, crate::permission::Decision::Allow { updated_input }) =
            (&verdict, &mut decision)
        {
            annotate(updated_input, answers, message);
        }
        let response = crate::permission::result(&decision);
        let line = json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": asked.request, "response": response },
        });
        self.writes.push(line.to_string());
        // "Yes, and tell Claude what to do next": the call goes ahead, and the person's words
        // go into the turn, which Claude Code takes at its next step.
        let next = words.filter(|_| edited.is_none());
        let settled = Settled::Answered { verdict, by };
        let mut outs =
            self.history.permission(&PermissionEvent::Settled { ask: number, outcome: settled });
        if let Some(next) = next {
            outs.extend(self.send(&next, &[], IntentId::new(), now));
        }
        outs.extend(self.status_now(WallMs::now()));
        Some(Answered { outs })
    }

    /// The plan approved with a clear context, as the TUI's "Yes, clear context" does it: the
    /// plan's call is turned down and the turn stopped, the mode `choice` names holds from then
    /// on (edits accepted, auto mode or bypass permissions), the context is cleared (`/clear`),
    /// and the plan goes as the next message.
    fn plan_cleared(
        &mut self,
        ask: &AskId,
        asked: &Asked,
        choice: &str,
        by: Answerer,
        now: WallMs,
    ) -> Answered {
        let mode = super::history::clear_mode(choice).unwrap_or("acceptEdits").to_owned();
        let mode = mode.as_str();
        let line = json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": asked.request, "response": {
                "behavior": "deny",
                "message": "The person approved the plan and cleared the context to carry it out.",
                "interrupt": true,
            } },
        });
        self.writes.push(line.to_string());
        let state = slopty_proto::thread::RequestState::Answered { by, choice: choice.to_owned() };
        let mut outs = self.history.resolved(ask, state);
        self.set_mode(mode);
        outs.extend(self.history.notice(Notice::MODE, &switched_to(mode), now));
        let plan = str_at(&asked.input, "plan").unwrap_or_default();
        let next = format!("Implement the following plan:\n\n{plan}");
        for text in ["/clear", next.as_str()] {
            let intent = IntentId::new();
            outs.extend(self.send(text, &[], intent, now));
        }
        outs.extend(self.status_now(now));
        Answered { outs }
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
        // A cleared conversation goes on in a session of its own, named by what comes next.
        if self.clearing
            && let Some(session) = str_at(message, "session_id")
            && session != self.history.meta().native
        {
            self.clearing = false;
            outs.extend(self.history.moved_to(session));
        }
        outs.extend(match kind {
            "conversation_reset" => {
                self.clearing = true;
                self.history.conversation_cleared()
            }
            "tool_progress" => self.tool_progress(message, now),
            "prompt_suggestion" => self.suggested(message),
            "active_goal" => self.goal(message, now),
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
        if control == Control::Initialize {
            self.starting = None;
        }
        let ok = str_at(response, "subtype") == Some("success");
        let said = response.get("response").cloned().unwrap_or(Value::Null);
        if !ok {
            let error = str_at(response, "error").unwrap_or("no reason given").to_owned();
            if self.quiet.remove(id) {
                return self.unanswered(&control, &error);
            }
            return self.refused(&control, &error, now);
        }
        self.quiet.remove(id);
        match control {
            Control::Initialize => self.initialized(response, &said, now),
            Control::Interrupt => {
                self.interrupting = None;
                if self.running {
                    self.stop_acked = Some(now);
                }
                Vec::new()
            }
            Control::SetModel(model) => {
                self.ask_settings();
                self.meters(|m| {
                    m.model = Some(super::model_label(&model));
                    m.model_id = Some(model);
                })
            }
            Control::SetMode(mode) => self.mode_now(mode, false, now),
            Control::Effort(effort) => {
                self.ask_settings();
                self.meters(|m| m.effort = Some(effort))
            }
            Control::Fast(on) => self.fast_now(if on { "on" } else { "off" }, None, now),
            Control::Thinking(on) => {
                let mut outs =
                    self.history.fact(THINKING_FACT, Some(if on { "on" } else { "off" }));
                outs.extend(self.passing(
                    "thinking",
                    if on { "Thinking on" } else { "Thinking off" },
                    now,
                ));
                outs
            }
            Control::Settings => self.settings_read(&said, now),
            Control::StopTask => Vec::new(),
            Control::ContextUsage => self.context_used(&said),
            Control::Background(call) => self.backgrounded(call.as_deref(), &said, now),
            Control::Rules => self.rules_listed(&said),
            Control::McpStatus => self.servers_listed(&said),
            Control::McpReconnect(_) | Control::McpEnable(..) => self.servers_changed(),
            Control::App(app) => {
                self.app_answered(app, &said);
                Vec::new()
            }
            Control::Rewind(turn, _) => self.rewound(turn, &said, now),
            Control::RewindPreview(preview, rest) => {
                self.rewind_previewed(preview, rest, &said);
                Vec::new()
            }
            Control::RewindConversation(turn) => self.conversation_rewound(turn, &said, now),
            Control::Ui(ui) => self.ui_answered(&ui, &said, now),
            Control::Side(asked) => self.side_answered(asked, &said),
            Control::Rename(title) => {
                self.custom_title = Some(title);
                Vec::new()
            }
        }
    }

    /// Claude Code refused `control`, saying `error`.
    fn refused(&mut self, control: &Control, error: &str, now: WallMs) -> Vec<Out> {
        if let Control::Rewind(turn, rest) = control {
            return self.rewind_refused(*turn, rest.clone(), error, now);
        }
        if matches!(control, Control::McpReconnect(_) | Control::McpEnable(..)) {
            // Its row says why, as the servers stand after it.
            let _asked = self.servers_changed();
        }
        // A switch that failed is said as the TUI says it.
        let said = match control {
            Control::SetModel(_) => Some(format!("Model switch failed: {error}")),
            Control::SetMode(mode) => {
                Some(format!("Couldn't switch to {}: {error}", mode_words(mode)))
            }
            Control::Effort(_) => {
                Some(format!("Effort setting unavailable right now · nothing changed: {error}"))
            }
            Control::Fast(_) => Some(format!("Fast mode is not available: {error}")),
            Control::Thinking(_) => Some(format!("Couldn't change thinking: {error}")),
            Control::McpReconnect(server) => {
                Some(format!("Failed to reconnect to {server}: {error}"))
            }
            Control::McpEnable(server, enabled) => {
                let verb = if *enabled { "enable" } else { "disable" };
                Some(format!("Couldn't {verb} {server}: {error}"))
            }
            _ => None,
        };
        if let Some(said) = said {
            return self.history.notice(Notice::INFO, &said, now);
        }
        let what = match control {
            Control::Initialize => "to start",
            Control::Interrupt => "to stop",
            Control::SetModel(_)
            | Control::SetMode(_)
            | Control::Effort(_)
            | Control::Fast(_)
            | Control::Thinking(_) => "the switch",
            Control::StopTask => "to stop the task",
            Control::Background(_) => "to move the call to the background",
            Control::Rewind(..) => "to put the files back",
            Control::RewindConversation(_) => "to take the conversation back",
            Control::Rules => "to list its permission rules",
            Control::McpStatus => "to list its MCP servers",
            Control::McpReconnect(_) | Control::McpEnable(..) => return Vec::new(),
            Control::App(_)
            | Control::RewindPreview(..)
            | Control::Ui(_)
            | Control::Side(_)
            | Control::Rename(_)
            | Control::ContextUsage
            | Control::Settings => {
                return self.unanswered(control, error);
            }
        };
        // A stop refused is a stop not acknowledged: it goes on to end the process
        // ([`INTERRUPT_ACK`]), and is never left at the refusal.
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
                    let mut options = Vec::new();
                    if bool_at(m, "supportsFastMode") {
                        options.push(fast_offer());
                    }
                    if bool_at(m, "supportsThinkingOff") {
                        options.push(thinking_offer());
                    }
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
            outs.extend(self.mode_now(mode.to_owned(), false, now));
        }
        outs.extend(self.fast_said(said, now));
        if let Some(agents) = said.get("agents").and_then(Value::as_array) {
            outs.extend(self.agents_listed(agents));
            outs.extend(self.agents_too_long(agents, now));
        }
        // What only a question answers: its rules, its servers' apps, the surfaces its mods
        // draw, and whether ultracode applies.
        self.after_initialize();
        self.ask_settings();
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
        match str_at(&request, "subtype") {
            Some("request_user_dialog") => return self.dialog(&id, &request, now),
            Some("elicitation") => return self.elicitation(&id, &request, now),
            _ => {}
        }
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
        let meters = self.history.meters_now();
        let context_used =
            meters.context_tokens.zip(meters.context_window).and_then(|(used, of)| {
                u32::try_from(used.saturating_mul(100).checked_div(of)?.min(100)).ok()
            });
        let needs_presence = bool_at(&request, "requires_user_interaction");
        let disk = on_disk(&tool, &input, &self.history.meta().cwd);
        let prompt = PermissionPrompt {
            needs_presence,
            reason: asked_by(asks::reason(&request, &tool, meters.mode.as_deref()), &request),
            ask: number,
            call: str_at(&request, "tool_use_id").map(str::to_owned),
            detail: conv::proposed(&tool, &input),
            tool: tool.clone(),
            // A request whose "always allow" Claude Code holds back offers none, and so does one
            // only the person at the card answers, or an overwrite they cannot review: no rule
            // answers it again without them.
            suggestions: if bool_at(&request, "suppress_always_allow_rule")
                || needs_presence
                || disk.as_ref().is_some_and(|d| d.one_time)
            {
                Vec::new()
            } else {
                crate::permission::suggestions(&suggestions)
            },
            mode: meters.mode.clone(),
            asked_ms: now,
            exists: written_exists(&tool, &input),
            on_disk: disk.map(|d| d.words),
            default_to_no: bool_at(&request, "default_to_no"),
            context_used,
            before_plan: self.before_plan(),
            input: input.clone(),
        };
        let form = asks::Form::Tool;
        self.asks.insert(number, Asked { request: id, tool, input, suggestions, form });
        let mut outs = self.history.permission(&PermissionEvent::Asked(Box::new(prompt)));
        outs.extend(self.status_now(now));
        outs
    }

    fn cancelled(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(id) = str_at(message, "request_id") else { return Vec::new() };
        let Some((&number, _)) = self.asks.iter().find(|(_, a)| a.request == id) else {
            return Vec::new();
        };
        let mut outs = self.withdraw(number);
        outs.extend(self.status_now(now));
        outs
    }

    /// Ask `number` is no longer asked.
    fn withdraw(&mut self, number: u64) -> Vec<Out> {
        let Some(asked) = self.asks.remove(&number) else { return Vec::new() };
        match asked.form {
            asks::Form::Tool => self
                .history
                .permission(&PermissionEvent::Settled { ask: number, outcome: Settled::Withdrawn }),
            _ => self.history.resolved(
                &AskId(number.to_string()),
                slopty_proto::thread::RequestState::Withdrawn,
            ),
        }
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
                let mut outs = self.servers_seen(message, now);
                outs.extend(self.init_problems(message, now));
                if let Some(model) = str_at(message, "model").map(str::to_owned) {
                    outs.extend(self.meters(|m| {
                        m.model = Some(super::model_label(&model));
                        m.model_id = Some(model);
                    }));
                }
                if let Some(mode) = str_at(message, "permissionMode").map(str::to_owned) {
                    outs.extend(self.mode_now(mode, true, now));
                }
                if let Some(version) = str_at(message, "claude_code_version") {
                    outs.extend(self.history.version(version));
                }
                outs
            }
            "status" => {
                let mut outs = Vec::new();
                if let Some(mode) = str_at(message, "permissionMode").map(str::to_owned) {
                    outs.extend(self.mode_now(mode, true, now));
                }
                let compacting = str_at(message, "status") == Some("compacting");
                outs.extend(self.history.compacting(compacting));
                if str_at(message, "compact_result") == Some("failed") {
                    let why = str_at(message, "compact_error").map(compact_failed);
                    let said = why.map_or_else(
                        || "Couldn't compact".to_owned(),
                        |why| format!("Couldn't compact · {why}"),
                    );
                    outs.extend(self.history.notice(Notice::INFO, &said, now));
                }
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
                // No answer at all, as the TUI says it: how long it waited.
                let text = match message.get("no_response").and_then(|n| u64_at(n, "waited_ms")) {
                    Some(waited) => {
                        format!("No response from the API after {}", work::span(waited.max(1_000)))
                    }
                    None => text,
                };
                self.history.retrying(&text, retry, now)
            }
            "task_started" => self.task_started(message, now),
            "task_progress" => self.task_progress(message, now),
            "task_updated" => self.task_updated(message, now),
            "task_notification" => self.task_notification(message, now),
            "background_tasks_changed" => self.tasks_changed(message, now),
            "task_summary" => self.phrase(message, now),
            "thinking_tokens" => self.thinking(message, now),
            "notification" => self.notification(message, now),
            "permission_denied" => self.denied(message, now),
            "commands_changed" => {
                let listed = message
                    .get("commands")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|c| {
                        serde_json::from_value::<crate::commands::Listed>(c.clone()).ok()
                    })
                    .collect();
                self.history.listed(listed)
            }
            // A page's form is done: Claude Code withdraws its request itself.
            "elicitation_complete" => Vec::new(),
            "ui_toast" | "ui_status" | "ui_log" | "ui_panes" | "ui_invalidate" => {
                self.ui_pushed(message, now)
            }
            subtype if work::derived(subtype) => self.derived_row(subtype, message, now),
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
            // The session's name, as given it in Claude Code; the person's from Slopty coming
            // back is theirs, kept on the thread's own mark.
            "session_title_changed" => match str_at(message, "title").map(str::trim) {
                Some(title) if !title.is_empty() => {
                    self.custom_title = Some(title.to_owned());
                    if title == self.person_named { Vec::new() } else { self.history.named(title) }
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// A user or assistant message: a record of the conversation, its subagent's when it names
    /// the call that started one.
    ///
    /// The stream tells a slash command's output before the echo of the command, which the
    /// transcript records first: what a command printed waits for the command it answers.
    /// Claude Code 2.1.302 tells that output as an assistant message of its own making that
    /// carries the command's words (`local_command_source`): it is read as those words, the
    /// command's output, as the transcript records it.
    fn record(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let printed = local_command_output(message);
        let message = printed.as_ref().unwrap_or(message);
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
        if printed.is_none() && str_at(message, "type") == Some("user") {
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
            self.note_calls(&record);
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
        let mut outs = if parent.is_none() { self.spin(event, now) } else { Vec::new() };
        outs.extend(self.streamed(event, parent, now));
        outs
    }

    /// What the model writes, as live blocks.
    fn streamed(&mut self, event: &Value, parent: Option<String>, now: WallMs) -> Vec<Out> {
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
        let said = self.limit_said(info, used, now);
        let used_bp = if rejected { 10_000 } else { used.map_or(0, basis_points) };
        let mut outs = self.meters(|m| {
            m.limits.retain(|l| l.name != name);
            m.limits.push(slopty_proto::thread::Limit { name, used_bp, resets_ms: resets });
        });
        outs.extend(said);
        if rejected {
            let first = self.limit_until.is_none();
            self.limit_until = resets.or(Some(now));
            outs.extend(self.history.limited(self.limit_until));
            if first {
                outs.extend(self.goal_paused(now));
            }
        }
        outs
    }

    /// What a usage limit's report says in passing, as the TUI's notifications do: a limit
    /// nearly used up ("You've used 80% of your weekly limit"), once a limit, and usage credits
    /// drawn on from now ("Now using usage credits").
    fn limit_said(&mut self, info: &Value, used: Option<f64>, now: WallMs) -> Vec<Out> {
        let mut outs = Vec::new();
        let kind = str_at(info, "rateLimitType").unwrap_or_default();
        if str_at(info, "status") == Some("allowed_warning")
            && self.said_once.0.insert(format!("limit:{kind}"))
        {
            let limit = match kind {
                "five_hour" => "5-hour limit",
                "seven_day" => "weekly limit",
                "seven_day_opus" => "Opus weekly limit",
                "seven_day_sonnet" => "Sonnet weekly limit",
                _ => "usage limit",
            };
            let words = match used.map(basis_points) {
                Some(bp) if bp > 0 => format!("You've used {}% of your {limit}", bp / 100),
                _ => format!("Approaching your {limit}"),
            };
            outs.extend(self.passing(&format!("limit-{kind}"), &words, now));
        }
        let credits = bool_at(info, "isUsingOverage");
        if credits && !self.said_once.1 {
            outs.extend(self.passing("credits", "Now using usage credits", now));
        }
        self.said_once.1 = credits;
        outs
    }

    fn result(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        // Whatever waited for an echo that never came goes before the turn ends.
        self.awaiting_echo = None;
        let mut outs = self.release_held(now);
        outs.extend(self.turn_over(now));
        let was_running = self.running;
        self.running = false;
        self.interrupting = None;
        self.stop_acked = None;
        outs.extend(self.history.stopping(None));
        outs.extend(self.history.compacting(false));
        let failed = bool_at(message, "is_error");
        let errors = || {
            let said: Vec<&str> = message
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            said.join("\n")
        };
        let interrupted = str_at(message, "subtype") == Some("error_during_execution")
            || str_at(message, "terminal_reason").is_some_and(|r| r.starts_with("aborted"));
        let end = if let Some(reason) = str_at(message, "startup_failure_reason") {
            // Claude Code would not start: why, in words, before what it printed.
            let words = format!("{STARTUP_REFUSED}: {}", startup_refused(reason));
            End::Failed(Some(format!("{words}\n{}", errors()).trim_end().to_owned()))
        } else if str_at(message, "terminal_reason") == Some("prompt_too_long") {
            let said = str_at(message, "result").map_or_else(errors, str::to_owned);
            End::Failed(Some(format!("{CONTEXT_FULL}\n{said}").trim_end().to_owned()))
        } else if interrupted {
            End::Interrupted
        } else if failed {
            End::Failed(str_at(message, "result").map(str::to_owned).filter(|r| !r.is_empty()))
        } else {
            End::Complete
        };
        if was_running || str_at(message, "local_command").is_none() {
            outs.extend(self.history.ended(&end, now));
        }
        // A command typed with its words (`/model sonnet`, `/effort max`) changed what the
        // session runs with in Claude Code itself: the meters ask what it now is.
        if matches!(str_at(message, "local_command"), Some("model" | "effort" | "config")) {
            self.ask_settings();
        }
        outs.extend(self.fast_said(message, now));
        // A hook held a call back: the turn stops at it until the person goes on.
        if let Some(deferred) = message.get("deferred_tool_use").filter(|d| d.is_object()) {
            let tool = str_at(deferred, "name").unwrap_or("a tool call");
            let words =
                format!("A hook deferred {tool} · the turn stopped here; send a message to go on");
            outs.extend(self.history.notice(Notice::HOOK, &words, now));
        }
        outs.extend(self.cache_expired(message, now));
        if let Some(model_usage) = message.get("modelUsage").and_then(Value::as_object) {
            let window = model_usage.values().filter_map(|u| u64_at(u, "contextWindow")).max();
            if window.is_some() {
                outs.extend(self.meters(|m| m.context_window = window));
            }
        }
        self.control(Control::ContextUsage, &json!({ "subtype": "get_context_usage" }));
        outs.extend(self.waiting_on_agents(now));
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
            let (kind, text) = match &asked.form {
                asks::Form::Tool if asked.tool == "AskUserQuestion" => {
                    ("question", super::history::asks_for(&asked.tool, &asked.input))
                }
                asks::Form::Tool => {
                    ("permission", super::history::asks_for(&asked.tool, &asked.input))
                }
                form => ("question", form.waits_with()),
            };
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
        let acked = self.stop_acked.filter(|_| self.running).map(|at| after(at, END_WAIT));
        let starting = self.starting.map(|at| after(at, self.init_wait));
        let delivery = self
            .sends
            .iter()
            .filter(|s| s.pending.state == PendingState::Sending)
            .map(|s| after(s.at, DELIVERY))
            .min();
        let limit = self.limit_until.map(|at| after(at, LIMIT_SLACK));
        let stall = self.running.then(|| after(self.looked.unwrap_or(self.heard), TICK));
        let tail = self.tails_due().map(|at| after(at, TAIL_EVERY));
        [interrupt, acked, starting, delivery, limit, stall, tail].into_iter().flatten().min()
    }

    /// Time has come to `now`. Whether the process is to be ended: a stop went unacknowledged.
    pub fn tick(&mut self, now: WallMs) -> (Vec<Out>, bool) {
        let mut outs = Vec::new();
        let past =
            |at: WallMs, wait: Duration| now.as_millis() >= at.as_millis().saturating_add(ms(wait));
        let mut escalate = self.interrupting.is_some_and(|at| past(at, INTERRUPT_ACK))
            || (self.running && self.stop_acked.is_some_and(|at| past(at, END_WAIT)));
        if self.starting.is_some_and(|at| past(at, self.init_wait)) {
            self.starting = None;
            escalate = true;
            let said = format!("{DID_NOT_START} in {} s", self.init_wait.as_secs());
            outs.extend(self.history.notice(Notice::INFO, &said, now));
        }
        // Messages not acknowledged: sent again once, then held as maybe not arrived. One read
        // again from the thread (a worker that restarted between sending it and hearing it) has
        // no line left, and goes straight to held once its time is up.
        let mut changed = false;
        for sent in &mut self.sends {
            if sent.pending.state != PendingState::Sending || !past(sent.at, DELIVERY) {
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
        if self.tails_due().is_some_and(|at| past(at, TAIL_EVERY)) {
            outs.extend(self.tails(now));
        }
        if self.running {
            let looked = self.looked.unwrap_or(self.heard);
            let woke = now.as_millis().saturating_sub(looked.as_millis()) > ms(WAKE_GAP);
            if woke {
                // The machine slept: the time since is not the turn's.
                self.heard = now;
            } else if past(self.heard, STALL) && !self.stalled {
                self.stalled = true;
                let said = match &self.last_call {
                    Some(call) => format!("{STALLED_TEXT} · last: {call}"),
                    None => STALLED_TEXT.to_owned(),
                };
                outs.extend(self.history.stalled(Some(said)));
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
    /// which starts Claude Code again on the session. `why` is said as an [`ENDED`] notice, in
    /// its own words ([`ending()`] words an end Claude Code chose).
    pub fn exited(&mut self, why: Option<&str>, now: WallMs) -> Vec<Out> {
        let mut outs = Vec::new();
        let numbers: Vec<u64> = self.asks.keys().copied().collect();
        for number in numbers {
            outs.extend(self.withdraw(number));
        }
        outs.extend(self.doors_gone(now));
        for sent in &mut self.sends {
            sent.line.clear();
            sent.pending.state = PendingState::Held { reason: Pending::UNKNOWN.to_owned() };
        }
        outs.extend(self.shown());
        if let Some(why) = why {
            outs.extend(self.history.notice(ENDED, why, now));
        }
        if self.running {
            // A stop asked for and not acknowledged ends the process: the turn ended stopped.
            let stopping = self.interrupting.is_some() || self.stop_acked.is_some();
            let end = match why {
                Some(why) if !stopping => End::Failed(Some(why.to_owned())),
                _ => End::Interrupted,
            };
            outs.extend(self.history.ended(&end, now));
        }
        self.running = false;
        self.interrupting = None;
        self.stop_acked = None;
        self.starting = None;
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

/// Thinking, as a model whose thinking can be turned off offers it.
fn thinking_offer() -> OptionOffer {
    OptionOffer {
        id: THINKING.to_owned(),
        label: "Thinking".to_owned(),
        choices: vec![
            OptionChoice { id: "off".to_owned(), label: "Off".to_owned() },
            OptionChoice { id: "on".to_owned(), label: "On".to_owned() },
        ],
        default: "on".to_owned(),
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

/// Why Claude Code asks, with the plugin whose run asks it named under it, as the TUI's dialog
/// names it ("Plugin deploy", or "From a plugin" when its name was not kept).
fn asked_by(reason: Option<String>, request: &Value) -> Option<String> {
    let plugin = request.get("plugin").and_then(Value::as_str).map(str::trim);
    let asker = plugin.map(|name| {
        if name.is_empty() { "From a plugin".to_owned() } else { format!("Plugin {name}") }
    });
    match (reason, asker) {
        (Some(reason), Some(asker)) => Some(format!("{reason}\n{asker}")),
        (reason, asker) => reason.or(asker),
    }
}

/// Context below which a cache that expired costs too little to say so, as the TUI's hint.
const UNCACHED_SAY_FROM: u64 = 50_000;

impl Driven {
    /// A turn after the first that read none of its context from the prompt cache and wrote
    /// much of it again: the cache expired while the conversation waited. The TUI warns of it
    /// before the next message ("~120k uncached · /clear to start fresh"); here it is said once
    /// it is known, from the turn's usage.
    fn cache_expired(&mut self, result: &Value, now: WallMs) -> Vec<Out> {
        let had_turn = std::mem::replace(&mut self.turned, true);
        let Some(usage) = result.get("usage") else { return Vec::new() };
        let read = u64_at(usage, "cache_read_input_tokens").unwrap_or(0);
        let written = u64_at(usage, "cache_creation_input_tokens").unwrap_or(0);
        if !had_turn || read > 0 || written < UNCACHED_SAY_FROM {
            return Vec::new();
        }
        let words = format!(
            "The prompt cache expired while the chat waited: ~{}k tokens went uncached · \
             /clear starts fresh",
            written / 1_000
        );
        self.passing("uncached", &words, now)
    }
}

/// How a turn's failure begins when Claude Code would not start, before its reason in words.
pub const STARTUP_REFUSED: &str = "Claude Code refused to start";

/// How a turn's failure begins when the conversation no longer fits the model's context, as the
/// TUI says it.
pub const CONTEXT_FULL: &str =
    "Context limit reached · compact or clear the conversation to continue";

/// Why Claude Code would not start (`result.startup_failure_reason`), in words: what to fix
/// rather than to try again.
fn startup_refused(reason: &str) -> String {
    match reason {
        "org_pin_api_key_conflict" => {
            "the organization's settings require its own sign-in, and an API key is set instead"
        }
        "provider_not_allowed" => "the managed settings do not allow this API provider",
        "org_verify_failed" => "the sign-in's organization could not be verified",
        "org_pin_mismatch" => "the sign-in belongs to an organization the settings do not allow",
        "managed_settings_invalid" => {
            "the managed settings could not be read, or they leave no model to use"
        }
        "remote_settings_required_unavailable" => {
            "the settings the organization requires could not be loaded"
        }
        "gateway_signin_required" => "the gateway ended this sign-in",
        "gateway_access_denied" => "the gateway refused this account",
        "proxy_invalid" => "a proxy setting is not a complete URL",
        "temp_dir_unusable" => "the temporary folder is unsafe or could not be made",
        "cwd_unavailable" => "the folder was deleted, moved or cannot be read",
        "shell_tool_missing" => "there is no shell tool: Git Bash and PowerShell are missing",
        "session_held_by_background" => "the conversation runs as a background session",
        "worktree_resume_refused" => "the session's worktree failed its safety checks",
        "worktree_unverified" => {
            "the session's worktree could not be verified now; trying again may work"
        }
        "cli_version_too_old" => "this Claude Code is older than Anthropic allows; update it",
        "bypass_root" => "bypass permissions was asked for while running as root",
        "org_config_required_unavailable" => {
            "the organization's policy could not be loaded; trying again may work"
        }
        "org_config_refused" => "Anthropic refused the organization's policy for this sign-in",
        other => return other.replace('_', " "),
    }
    .to_owned()
}

/// Why a compaction failed, as the TUI says it: its two known causes in short words, anything
/// else as Claude Code said it.
fn compact_failed(error: &str) -> String {
    if error.contains("could not be reduced below the context limit") {
        "conversation too long · /clear to start fresh".to_owned()
    } else if error.contains("attached media exceeds size limits") {
        "attached media too large · /clear to start fresh".to_owned()
    } else {
        error.trim().to_owned()
    }
}

/// Whether the file a `Write` call names is there already: the one look at the disk this module
/// makes, so the card can say "Create file" or "Overwrite file" as the TUI's does. `None` for
/// any other call, or a path that cannot be looked at.
fn written_exists(tool: &str, input: &Value) -> Option<bool> {
    if tool != "Write" {
        return None;
    }
    let path = str_at(input, "file_path").filter(|p| p.starts_with('/'))?;
    std::path::Path::new(path).try_exists().ok()
}

/// The `annotations` an answered `AskUserQuestion` input carries, as the TUI's dialog writes
/// them: by each question's text, the markdown preview of the option picked and the person's
/// notes (`notes`: a JSON object of notes by question text).
fn annotate(input: &mut Value, answers: &[conv::Answer], notes: Option<&str>) {
    let notes: serde_json::Map<String, Value> = notes
        .and_then(|n| serde_json::from_str::<Value>(n).ok())
        .and_then(|n| n.as_object().cloned())
        .unwrap_or_default();
    let mut annotations = serde_json::Map::new();
    let questions = input.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
    for question in &questions {
        let Some(text) = str_at(question, "question") else { continue };
        let picked = answers.iter().find(|a| a.question == text).map(|a| a.answer.as_str());
        let preview = question
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|option| picked.is_some() && str_at(option, "label") == picked)
            .and_then(|option| str_at(option, "preview"))
            .filter(|p| !p.trim().is_empty());
        let note = notes.get(text).and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty());
        let mut said = serde_json::Map::new();
        if let Some(preview) = preview {
            said.insert("preview".to_owned(), Value::String(preview.to_owned()));
        }
        if let Some(note) = note {
            said.insert("notes".to_owned(), Value::String(note.to_owned()));
        }
        if !said.is_empty() {
            annotations.insert(text.to_owned(), Value::Object(said));
        }
    }
    if !annotations.is_empty()
        && let Some(fields) = input.as_object_mut()
    {
        fields.insert("annotations".to_owned(), Value::Object(annotations));
    }
}

/// What "Chat about this" tells the model, in the TUI's words: the person wants to talk the
/// questions over first, and which they were.
fn clarify(input: &Value) -> String {
    let asked: Vec<String> = input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|q| str_at(q, "question"))
        .map(|q| format!("- \"{q}\"\n  (No answer provided)"))
        .collect();
    format!(
        "The user wants to clarify these questions.\n    This means they may have additional \
         information, context or questions for you.\n    Take their response into account and \
         then reformulate the questions if appropriate.\n    Start by asking them what they \
         would like to clarify.\n    Questions asked:\n{}",
        asked.join("\n")
    )
}

/// The most of an existing file the TUI reads to preview an overwrite (its `CQe`, 256 KiB).
const PREVIEW_BYTES: u64 = 262_144;

/// What the worker saw on disk where a call would write, in the TUI's words.
struct OnDisk {
    words: String,
    /// The overwrite cannot be reviewed, so approval is one-time only.
    one_time: bool,
}

/// What is worth saying about where `tool` would write: a symlink whose target leaves `cwd`
/// (any writing tool), an existing file too large to preview, or one whose contents cannot be
/// shown in full (a `Write` over it), as the TUI's permission card says.
fn on_disk(tool: &str, input: &Value, cwd: &str) -> Option<OnDisk> {
    let key = match tool {
        "Write" | "Edit" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    let path = std::path::Path::new(str_at(input, key).filter(|p| p.starts_with('/'))?);
    let mut said = Vec::new();
    let mut one_time = false;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
        && let Ok(target) = std::fs::canonicalize(path)
    {
        let inside = std::fs::canonicalize(cwd).is_ok_and(|root| target.starts_with(root));
        let target = target.display();
        said.push(if inside {
            format!("Symlink target: {target}")
        } else {
            format!("This will modify {target} (outside working directory) via a symlink")
        });
    }
    if tool == "Write"
        && let Ok(meta) = std::fs::metadata(path)
        && meta.is_file()
    {
        let shown = path.display();
        if meta.len() > PREVIEW_BYTES {
            said.push(format!(
                "Existing file is too large to preview — approving will overwrite {shown}."
            ));
        } else if std::fs::read(path).is_ok_and(|bytes| !showable(&bytes)) {
            one_time = true;
            said.push(format!(
                "Current contents of {shown} cannot be shown in full — the overwrite cannot be \
                 reviewed, so approval is one-time only (deny unless expected)."
            ));
        }
    }
    (!said.is_empty()).then(|| OnDisk { words: said.join("\n"), one_time })
}

/// Whether `bytes` read as text a card can show whole: UTF-8 without control characters
/// besides line breaks and tabs.
fn showable(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .is_ok_and(|text| !text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
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

/// A command's output Claude Code told as an assistant message of its own (`model`
/// `<synthetic>`, its words in `local_command_source`), as the user record the transcript keeps
/// it as: `<local-command-stdout>…</local-command-stdout>`. `None` for any other message.
fn local_command_output(message: &Value) -> Option<Value> {
    if str_at(message, "type") != Some("assistant")
        || message.get("parent_tool_use_id").is_some_and(|p| !p.is_null())
    {
        return None;
    }
    let source = str_at(message, "local_command_source")?;
    let mut record = serde_json::Map::new();
    record.insert("type".to_owned(), "user".into());
    record.insert("message".to_owned(), json!({ "role": "user", "content": source }));
    record.insert("parent_tool_use_id".to_owned(), Value::Null);
    record.insert("isReplay".to_owned(), true.into());
    for key in ["uuid", "session_id", "timestamp"] {
        if let Some(value) = message.get(key) {
            record.insert(key.to_owned(), value.clone());
        }
    }
    Some(Value::Object(record))
}

#[cfg(test)]
mod tests;
