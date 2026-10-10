//! Claude Code, observed: its transcript, hooks, status line and mod mapped onto the
//! agent-neutral thread model (`slopty_proto::thread`).
//!
//! Sans-IO. The worker reads the files and hears the hooks and the mod; this turns what they
//! say into [`Out`]s, the threads to make and the actions to apply, and keeps only what the
//! mapping needs (which turn each entry is in, which subagent hangs off which call).
//!
//! - **Threads.** A Claude Code session is one thread, its id derived from the session's own
//!   ([`thread_of`]), so the same session is the same thread across a worker restart and a `claude
//!   --resume` in another terminal. Each subagent is a thread of its own, derived from the
//!   session's and its agent id, linked to the `Agent` call that started it.
//! - **Turns.** A prompt starts the next turn of its thread; every entry after it is in that turn.
//!   The transcript's turn record fills in the models, the tokens and the end. A branch (a rewind,
//!   an edited prompt) truncates the thread back to before the dropped prompt.
//! - **Items.** An entry keeps its id. A tool call's kind comes from the decoder's typed detail,
//!   not from Claude Code's tool names, and its title is worded here.
//! - **Live blocks.** Where the mod is heard, a block the model is writing is an item of its own,
//!   appended to as it grows, and removed once the transcript's entry settles it
//!   ([`crate::live::Overlay`], which this keeps for the thread). A tool call's live block takes
//!   the call's own id, so the transcript's entry replaces it. A block goes in its thread's open
//!   turn; one that begins before the transcript has its prompt (the hook and the mod run ahead of
//!   the file) waits for that prompt, with what it says meanwhile, so it never stands in the turn
//!   before.
//! - **Requests.** A permission prompt the worker holds is a request with the answers Claude Code
//!   takes: allow, allow always (with what the suggestions grant), deny, and deny and stop.
//!   [`verdict`] maps a chosen answer back. A prompt is held only through the `PermissionRequest`
//!   hook, so the thread declares `approvals` once a hook has been heard and not before: a Claude
//!   Code thread without it is one whose hooks are not installed.
//! - **Auto mode's declines.** A call auto mode's classifier turned down is said by a notice
//!   ([`Notice::DECLINED`]) right after the call, whose own entry ends as any failure does. When
//!   the person may let the model try it again, the worker holds that as a request
//!   ([`Request::RETRY`]): "Let it try again" allows, "Keep it declined" denies.
//! - **Before the session id.** A Claude Code started by hand and idle at its prompt has no session
//!   id yet (no hook, no transcript). Its terminal names a provisional thread
//!   ([`Observed::provisional`], [`terminal_thread`]), which gives way to the session's own once
//!   the id is known.
//! - **One session in two terminals.** Two live Claude Codes on one session id (a `--resume` of a
//!   session still running elsewhere) are told apart by their terminals: the first keeps the
//!   session's thread, the second gets one of its own ([`Observed::beside`], [`thread_in`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use slopty_core::{ClientId, SessionId, WallMs};
use slopty_proto::thread::{
    self, Action, AgentId, Answerer, AskId, BackgroundTask, Cap, Changed, Choice, Clipped,
    Compaction, ContentRef, Drive, Effect, Item, ItemBody, ItemId, Limit, Link, Liveness, Meters,
    Model, Notice, PartKey, Phase, Plan, Request, RequestState, Retry, Status, Step, ThreadId,
    ThreadMeta, ToolCall, ToolState, Turn, TurnId, TurnState, Usage, UserMessage, Wait, detail,
    kind,
};

use crate::conversation::{
    self as conv, Body, Change, Grant, Live, LiveId, LiveKind, NoteKind, PermissionEvent,
    PermissionPrompt, ResultStatus, Settled, TextRef, Verdict,
};
use crate::live;
use crate::status::{AgentEvent, AgentSource, AgentStatus, BlockReason};

/// What the threads observed need done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Host this thread from nothing: a new one, or one held from before (a worker restart,
    /// a transcript that started over), whose log starts over.
    Begin(Box<ThreadMeta>),
    /// Apply actions to a thread.
    Actions(ThreadId, Vec<Action>),
}

/// The capabilities an observed Claude Code has through Slopty before its hooks are heard;
/// [`Cap::APPROVALS`] joins them once they are.
pub const CAPS: [&str; 8] = [
    Cap::FORK,
    Cap::INTERRUPT,
    Cap::LIVE_TUI,
    Cap::QUEUE,
    Cap::SET_MODEL,
    Cap::SCHEDULE,
    Cap::SNAPSHOTS,
    Cap::STEER,
];

/// Claude Code's own review ([`Cap::REVIEW`]): its built-in skill, which takes a ref range
/// (`a...b`) and reviews what it changes. A thread can review only while Claude Code lists it.
pub const REVIEW_COMMAND: &str = "code-review";

/// The models `/model` takes, by the aliases Claude Code resolves to its current ones, until
/// the mod lists Claude Code's own ([`Observed::catalog`]); where it is not heard, these stand.
pub const MODELS: [(&str, &str); 4] =
    [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku"), ("default", "Default")];

/// A model alias for people: `sonnet[1m]` is "Sonnet 1M", `opusplan` "Opusplan".
fn alias_label(alias: &str) -> String {
    let (base, wide) = match alias.strip_suffix("[1m]") {
        Some(base) => (base, " 1M"),
        None => (alias, ""),
    };
    let mut chars = base.chars();
    let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    format!("{first}{}{wide}", chars.as_str())
}

/// What a permission waits on, as a row leads with it: the action on its subject ("Run cargo
/// test", [`tool_action`]) from the hook's line `detail`, or what the tool does when the hook
/// named only the tool ([`tool_statement`]). A line with no tool is said as it is.
fn permission_words(tool: &str, detail: Option<&str>) -> String {
    let line = detail.map(str::trim).filter(|d| !d.is_empty());
    match (tool, line) {
        ("", line) => line.unwrap_or_default().to_owned(),
        (tool, line) => {
            let subject = line.map(|l| ask_subject(tool, l)).unwrap_or_default();
            if subject.is_empty() { tool_statement(tool) } else { tool_action(tool, subject) }
        }
    }
}

/// Using `tool` on `subject`, said as the action: "Run cargo test", "Edit src/main.rs", "Use
/// query from db: users".
#[must_use]
fn tool_action(tool: &str, subject: &str) -> String {
    let verb = match tool {
        "Bash" => "Run",
        "Edit" | "MultiEdit" | "NotebookEdit" => "Edit",
        "Write" => "Write",
        "Read" => "Read",
        "WebFetch" => "Fetch",
        "WebSearch" => "Search",
        "Task" | "Agent" => "Start a subagent:",
        "Skill" => "Use",
        _ => {
            let mcp = tool.strip_prefix("mcp__").and_then(|rest| rest.split_once("__"));
            return match mcp {
                Some((server, name)) => format!("Use {name} from {server}: {subject}"),
                None => format!("Use {tool}: {subject}"),
            };
        }
    };
    format!("{verb} {subject}")
}

/// What using `tool` asks, as the approval card says it without the subject: "Wants to run a
/// command", "Wants to edit a file", "Wants to use query from db".
///
/// The card names Claude and the file ("Claude wants to edit main.rs"); a row that knows only
/// the tool says the kind of thing, and its meta says whose agent asks.
#[must_use]
fn tool_statement(tool: &str) -> String {
    let what = match tool {
        "Bash" => "run a command",
        "Edit" | "MultiEdit" | "NotebookEdit" => "edit a file",
        "Write" => "write a file",
        "Read" => "read a file",
        "WebFetch" => "fetch a page",
        "WebSearch" => "search the web",
        "Task" | "Agent" => "start a subagent",
        "ExitPlanMode" => "leave plan mode",
        _ => {
            let mcp = tool.strip_prefix("mcp__").and_then(|rest| rest.split_once("__"));
            return match mcp {
                Some((server, name)) => format!("Wants to use {name} from {server}"),
                None => format!("Wants to use {tool}"),
            };
        }
    };
    format!("Wants to {what}")
}

/// A tool call's line with the name that leads it dropped: "$ ", or a first word the tool's
/// name ends with ("Edit" for Edit, "Fetch" for `WebFetch`, "Agent:" for Task).
fn ask_subject<'a>(tool: &str, line: &'a str) -> &'a str {
    let line = line.strip_prefix("$ ").unwrap_or(line);
    if line == tool {
        return "";
    }
    let word_end = line.find([' ', ':']).unwrap_or(line.len());
    let (word, rest) = line.split_at(word_end);
    let named = word.starts_with(char::is_uppercase)
        && (tool.ends_with(word) || (word == "Agent" && tool == "Task"));
    if named { rest.trim_start_matches(':').trim_start() } else { line }
}

/// What an agent stopped on its plan's usage limit waits on ([`Wait::LIMIT`]).
pub const LIMIT_TEXT: &str = "Hit its usage limit";

/// Who answered a request the agent asked in its own terminal ([`Answerer::name`]).
pub const IN_TERMINAL: &str = "terminal";

/// How long a block waits for a prompt held here before it is asked in the terminal.
///
/// The held prompt follows its block's status at once, and the worker looks again on each of
/// its ticks ([`Observed::waited`]).
pub const ASK_GRACE: std::time::Duration = std::time::Duration::from_millis(400);

/// How long a Claude Code Slopty started may stay silent before the thread says it asks
/// something in its terminal ([`Observed::unheard`]).
///
/// Its hooks run only once its own dialogs at start (the folder's trust, a project's
/// `.mcp.json`) are answered, so their silence is the sign one waits; the screen is never read
/// for it.
pub const UNHEARD: std::time::Duration = std::time::Duration::from_secs(5);

/// What a Claude Code held at a dialog of its own at start asks, as its thread says it.
pub const ASKING_IN_TERMINAL: &str = "Claude Code is asking something in its terminal";

/// The request of a Claude Code held at a dialog of its own at start ([`Observed::unheard`]).
pub const UNHEARD_ASK: &str = "terminal-start";

/// The choice on [`UNHEARD_ASK`] that trusts the start's folder for Claude Code and starts it
/// again past its trust dialog (`slopty_agent::trust::trust_named`).
pub const TRUST_CHOICE: &str = "trust";

/// What [`TRUST_CHOICE`] says.
pub const TRUST_LABEL: &str = "Trust this folder";

/// The kind of [`UNHEARD_ASK`]: input the agent waits for in its terminal.
const INPUT: &str = "input";

/// The request a block that began at `since` asks in the agent's own terminal.
fn terminal_ask(since: WallMs) -> AskId {
    AskId(format!("terminal-{}", since.as_millis()))
}

/// The thread of Claude Code session `native`.
#[must_use]
pub fn thread_of(native: &str) -> ThreadId {
    ThreadId::derived(&["claude-code session", native])
}

/// The thread of Claude Code session `native` as terminal `terminal` runs it while another
/// terminal's Claude Code runs the same session: each terminal keeps a thread of its own.
#[must_use]
pub fn thread_in(native: &str, terminal: SessionId) -> ThreadId {
    ThreadId::derived(&["claude-code session", native, "in terminal", &terminal.to_string()])
}

/// The provisional thread of the Claude Code in terminal `terminal`, before its session id is
/// known.
#[must_use]
pub fn terminal_thread(terminal: SessionId) -> ThreadId {
    ThreadId::derived(&["claude-code terminal", &terminal.to_string()])
}

/// Where the whole of a clipped text is: the decoder's thread and reference, as JSON.
#[must_use]
pub fn content_ref(thread: &conv::ThreadId, at: &TextRef) -> ContentRef {
    ContentRef(serde_json::to_string(&(thread, at)).unwrap_or_default())
}

/// The decoder's thread and reference a [`content_ref`] names.
#[must_use]
pub fn text_ref(content: &ContentRef) -> Option<(conv::ThreadId, TextRef)> {
    serde_json::from_str(&content.0).ok()
}

/// The answer to give Claude Code for choice `choice` of a request made from a permission
/// prompt, with the person's `message`; `None` for a choice it never offered.
#[must_use]
pub fn verdict(choice: &str, message: Option<&str>) -> Option<Verdict> {
    let message = message.unwrap_or_default().to_owned();
    match choice {
        "allow" => Some(Verdict::Allow),
        "always" => Some(Verdict::AllowAlways),
        "deny" => Some(Verdict::Deny { message, interrupt: false }),
        "deny-stop" => Some(Verdict::Deny { message, interrupt: true }),
        answers => serde_json::from_str(answers).ok().map(|answers| Verdict::Answer { answers }),
    }
}

/// The choice a verdict took, as [`verdict`] reads it.
fn choice_of(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Allow => "allow".to_owned(),
        Verdict::AllowAlways => "always".to_owned(),
        Verdict::Deny { interrupt: false, .. } => "deny".to_owned(),
        Verdict::Deny { interrupt: true, .. } => "deny-stop".to_owned(),
        Verdict::Answer { answers } => serde_json::to_string(answers).unwrap_or_default(),
    }
}

/// One of the decoder's threads, as mapped.
#[derive(Debug)]
struct Mapped {
    id: ThreadId,
    /// The turn entries now go to.
    turn: TurnId,
    /// Each prompt's turn.
    prompts: HashMap<String, TurnId>,
    /// Each entry's turn and the lines it changed.
    items: HashMap<String, (TurnId, Changed)>,
    /// Turns an interrupt ended.
    interrupted: HashSet<TurnId>,
    /// Turns an API error Claude Code gave up on ended, with what it said: a turn the model
    /// went on in after one is not among them.
    stopped: HashMap<TurnId, (conv::Stop, String)>,
    /// Turns whose end was sent.
    ended: HashSet<TurnId>,
    /// Each turn as last sent.
    turns: HashMap<TurnId, Turn>,
    /// Background commands as last sent, for their output to land on.
    background: HashMap<String, Item>,
    /// Calls not ended yet, as last sent, by call: a permission prompt held for one marks it
    /// waiting on the person, and its settling lets it run again.
    calls: HashMap<String, Item>,
    /// The work left running in the background, by the call that started it, in the order
    /// the calls came, as last sent.
    tasks: Vec<BackgroundTask>,
}

impl Mapped {
    fn new(id: ThreadId) -> Self {
        Self {
            id,
            turn: TurnId::BEFORE,
            prompts: HashMap::new(),
            items: HashMap::new(),
            interrupted: HashSet::new(),
            stopped: HashMap::new(),
            ended: HashSet::new(),
            turns: HashMap::new(),
            background: HashMap::new(),
            calls: HashMap::new(),
            tasks: Vec::new(),
        }
    }

    /// Put `task` in the list in place of the one its call started before, or drop that one
    /// when the call starts none now; whether the list changed.
    fn set_task(&mut self, call: &str, task: Option<BackgroundTask>) -> bool {
        let at = self.tasks.iter().position(|t| t.item.as_ref().is_some_and(|i| i.0 == call));
        match (at, task) {
            (Some(at), Some(task)) => match self.tasks.get_mut(at) {
                Some(have) if *have != task => {
                    *have = task;
                    true
                }
                _ => false,
            },
            (Some(at), None) => {
                self.tasks.remove(at);
                true
            }
            (None, Some(task)) => {
                self.tasks.push(task);
                true
            }
            (None, None) => false,
        }
    }

    fn changed(&self, turn: TurnId) -> Changed {
        self.items.values().filter(|(t, _)| *t == turn).fold(Changed::default(), |sum, (_, c)| {
            Changed {
                added: sum.added.saturating_add(c.added),
                removed: sum.removed.saturating_add(c.removed),
            }
        })
    }
}

/// A live block as an item.
#[derive(Debug)]
struct Provisional {
    thread: conv::ThreadId,
    item: ItemId,
    tool: bool,
}

/// A live block that began while its thread had no turn open: its prompt is not in the
/// transcript yet. It is held, with what it said meanwhile, until that prompt opens its turn.
#[derive(Debug)]
struct Deferred {
    thread: conv::ThreadId,
    id: LiveId,
    kind: LiveKind,
    text: String,
}

/// What an agent at rest waits on, as its tracker said: the tasks left running, the scheduled
/// prompts, and the hook's words for the first task.
#[derive(Debug)]
struct Waiting {
    tasks: u32,
    crons: u32,
    named: Option<String>,
}

/// One observed Claude Code session.
#[derive(Debug)]
pub struct Observed {
    meta: ThreadMeta,
    threads: HashMap<conv::ThreadId, Mapped>,
    /// Each subagent's call: the decoder's thread it is in, and its id.
    spawned: HashMap<String, (conv::ThreadId, String)>,
    overlay: live::Overlay,
    live: HashMap<LiveId, Provisional>,
    /// Live blocks held until their prompt opens their turn, oldest first.
    deferred: Vec<Deferred>,
    meters: Meters,
    /// The status last told, and the requests open: a thread begun anew from the transcript
    /// gets them again, since the transcript has neither.
    status: Option<Status>,
    open: Vec<Request>,
    /// What the agent asks in its own terminal with no prompt held here (nobody followed it,
    /// or no relay held it): a request answered only there, so the thread still shows it.
    in_terminal: Option<Request>,
    /// What a Claude Code Slopty started asks in its terminal before any hook spoke
    /// ([`Observed::unheard`]): open until the first hook.
    unheard: Option<Request>,
    /// When the block a held prompt stood for began: that block is the prompt's, and asks
    /// nothing more in the terminal once the prompt is settled.
    held_block: Option<WallMs>,
    /// The kind of request the agent's block asks for, while it is blocked on the person.
    blocked_kind: Option<&'static str>,
    /// What the agent waits on at rest, as the tracker counts it (background tasks, scheduled
    /// prompts), while it waits: the wait is worded again as the transcript names those tasks.
    waiting_on: Option<Waiting>,
    out: Vec<Out>,
    /// The title is the session's own name, which its first prompt replaces.
    named: bool,
    /// A hook has been heard, so prompts are held and `approvals` is declared.
    hooked: bool,
    /// The slash commands as last told.
    commands: Vec<thread::Command>,
    /// The person's and the project's own commands on disk (`crate::commands::custom`).
    custom: Vec<conv::SlashCommand>,
    /// Claude Code's own lists, as the mod last sent them.
    catalog: Option<live::Catalog>,
    /// The main thread's last turn ended failed, so its agent's done is a failure.
    failed: bool,
    /// Auto mode's declines of calls the transcript does not have yet, by call: each is said
    /// once its call is.
    declines: HashMap<String, (Notice, WallMs)>,
}

impl Observed {
    /// Session `native` of Claude Code `version`, running in terminal `terminal` in `cwd`.
    /// The first outs it gives, from whichever call, begin its thread.
    #[must_use]
    pub fn new(
        native: &str,
        version: &str,
        terminal: Option<SessionId>,
        cwd: &str,
        now: WallMs,
    ) -> Self {
        Self::begun(thread_of(native), native, version, terminal, cwd, now)
    }

    /// Session `native` as terminal `terminal` runs it while another terminal's Claude Code
    /// runs it too ([`thread_in`]): a thread of this terminal's own, so neither moves the other's.
    #[must_use]
    pub fn beside(
        native: &str,
        version: &str,
        terminal: SessionId,
        cwd: &str,
        now: WallMs,
    ) -> Self {
        Self::begun(thread_in(native, terminal), native, version, Some(terminal), cwd, now)
    }

    /// The Claude Code in terminal `terminal` whose session id is not known yet: a thread
    /// named by the terminal, with no native id, until [`Observed::new`] takes over.
    #[must_use]
    pub fn provisional(version: &str, terminal: SessionId, cwd: &str, now: WallMs) -> Self {
        Self::begun(terminal_thread(terminal), "", version, Some(terminal), cwd, now)
    }

    fn begun(
        id: ThreadId,
        native: &str,
        version: &str,
        terminal: Option<SessionId>,
        cwd: &str,
        now: WallMs,
    ) -> Self {
        let mut caps: Vec<Cap> = CAPS.iter().map(|c| Cap::named(c)).collect();
        caps.sort();
        let meta = ThreadMeta {
            modes: Vec::new(),
            efforts: Vec::new(),
            id,
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: version.to_owned(),
            native: native.to_owned(),
            cwd: cwd.to_owned(),
            title: String::new(),
            terminal,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::OBSERVED),
            caps,
            models: MODELS
                .iter()
                .map(|(id, label)| Model { id: (*id).to_owned(), label: (*label).to_owned() })
                .collect(),
            facts: BTreeMap::new(),
            created_ms: now,
        };
        let mut threads = HashMap::new();
        threads.insert(conv::ThreadId::Main, Mapped::new(id));
        Self {
            out: vec![Out::Begin(Box::new(meta.clone()))],
            meta,
            threads,
            spawned: HashMap::new(),
            overlay: live::Overlay::default(),
            live: HashMap::new(),
            deferred: Vec::new(),
            meters: Meters::default(),
            status: None,
            open: Vec::new(),
            in_terminal: None,
            unheard: None,
            held_block: None,
            blocked_kind: None,
            waiting_on: None,
            named: false,
            hooked: false,
            commands: Vec::new(),
            custom: Vec::new(),
            catalog: None,
            failed: false,
            declines: HashMap::new(),
        }
    }

    /// Whether the session id is still unknown ([`Observed::provisional`]).
    #[must_use]
    pub const fn is_provisional(&self) -> bool {
        self.meta.native.is_empty()
    }

    /// A hook spoke for the session: prompts are held from now on, so the thread declares
    /// `approvals`.
    pub fn hooked(&mut self) -> Vec<Out> {
        self.hear_hook();
        self.drain()
    }

    fn hear_hook(&mut self) {
        if self.hooked {
            return;
        }
        self.hooked = true;
        let approvals = Cap::named(Cap::APPROVALS);
        if let Err(at) = self.meta.caps.binary_search(&approvals) {
            self.meta.caps.insert(at, approvals);
        }
        self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        // Its hooks speak once its own dialog is answered, in its terminal.
        self.answered_in_terminal();
    }

    /// The dialog Claude Code was held at ([`Self::unheard`]) was answered in its terminal:
    /// its hooks spoke, or a turn began after it opened.
    fn answered_in_terminal(&mut self) {
        if let Some(asked) = self.unheard.take() {
            let by = Answerer { client: None, name: IN_TERMINAL.to_owned() };
            let state = RequestState::Answered { by, choice: String::new() };
            self.push(self.meta.id, Action::RequestResolved { id: asked.id, state });
            let status = self.status.clone().unwrap_or(Status {
                phase: Phase::Idle,
                wait: None,
                liveness: Liveness::Live,
                since_ms: asked.opened_ms,
            });
            self.push(self.meta.id, Action::Status(status));
        }
    }

    /// A Claude Code Slopty started, silent since it opened [`UNHEARD`] ago at `now`: it is held
    /// at a dialog of its own (the folder's trust, a project's `.mcp.json`), which its hooks
    /// wait behind. The thread asks the person to answer it there ([`ASKING_IN_TERMINAL`]), a
    /// request answered in the terminal, until the first hook. `trust` names the folder whose
    /// trust Claude Code keeps for the start when that is not kept yet and the person may give
    /// it here: the request then offers [`TRUST_CHOICE`] too, with the folder as its reach.
    pub fn unheard(&mut self, now: WallMs, trust: Option<&str>) -> Vec<Out> {
        if self.hooked || self.unheard.is_some() || self.meta.terminal.is_none() {
            return self.drain();
        }
        let request = Request {
            id: AskId(UNHEARD_ASK.to_owned()),
            item: None,
            kind: INPUT.to_owned(),
            title: ASKING_IN_TERMINAL.to_owned(),
            text: None,
            options: trust
                .map(|folder| Choice {
                    id: TRUST_CHOICE.to_owned(),
                    label: TRUST_LABEL.to_owned(),
                    effect: Effect::Allow,
                    scope: Some(folder.to_owned()),
                    stops: false,
                })
                .into_iter()
                .collect(),
            questions: Vec::new(),
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: None,
        };
        self.unheard = Some(request.clone());
        self.push(self.meta.id, Action::RequestOpened(Box::new(request)));
        let status = self.status.clone().unwrap_or(Status {
            phase: Phase::Idle,
            wait: None,
            liveness: Liveness::Live,
            since_ms: now,
        });
        if let Some(status) = self.shown(Some(status)) {
            self.push(self.meta.id, Action::Status(status));
        }
        self.drain()
    }

    /// `status` as the thread shows it: waiting on the person while its agent is held at a
    /// dialog of its own ([`Self::unheard`]) and runs.
    fn shown(&self, status: Option<Status>) -> Option<Status> {
        let mut status = status?;
        if let Some(asked) = self.unheard.as_ref().filter(|_| status.liveness == Liveness::Live) {
            status.phase = Phase::NeedsYou;
            status.wait = Some(Wait { kind: INPUT.to_owned(), text: asked.title.clone() });
            status.since_ms = asked.opened_ms;
        }
        Some(status)
    }

    /// The session's own thread.
    #[must_use]
    pub const fn main(&self) -> ThreadId {
        self.meta.id
    }

    /// Its metadata as it stands.
    #[must_use]
    pub const fn meta(&self) -> &ThreadMeta {
        &self.meta
    }

    /// What is due and not yet given: at first, the thread's beginning.
    pub fn drain(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.out)
    }

    /// What the transcript's `changes` (and the background commands' `outputs`) make of the
    /// threads; live blocks the changes settle go after them.
    ///
    /// A turn's record goes after the batch's entries: the decoder keeps one record a turn per
    /// batch, where the turn first changed, so the entries that ended it (an interrupt, the
    /// error it stopped on) come after it.
    pub fn transcript(&mut self, changes: &[Change], outputs: &[conv::Output]) -> Vec<Out> {
        let (turns, entries): (Vec<&Change>, Vec<&Change>) =
            changes.iter().partition(|c| matches!(c, Change::Turn { .. }));
        for change in entries.into_iter().chain(turns) {
            match change {
                Change::Reset { thread } => self.reset(thread.as_ref()),
                Change::Upsert { thread, entry } => self.entry(thread, entry),
                Change::Remove { thread, id } => self.remove(thread, id),
                Change::Tasks { thread, tasks } => {
                    let plan = (!tasks.is_empty())
                        .then(|| Plan { text: None, steps: tasks.iter().map(step).collect() });
                    let id = self.ensure(thread);
                    self.push(id, Action::PlanSet(plan));
                }
                Change::Turn { thread, turn } => self.turn(thread, turn),
            }
        }
        for output in outputs {
            self.output(output);
        }
        let cleared = self.overlay.settle(changes);
        self.cleared(&cleared);
        self.drain()
    }

    /// The mod's board at `now` (`wall` by the wall clock): blocks started, grown and let go.
    pub fn live(&mut self, board: &live::Board, now: Instant, wall: WallMs) -> Vec<Out> {
        let mut changes = self.overlay.update(board, now, wall);
        changes.extend(self.overlay.expire(now));
        for change in changes {
            match change {
                Live::Start { thread, id, kind } => self.live_start(&thread, id, &kind),
                Live::Append { id, text } => {
                    if let Some(held) = self.deferred.iter_mut().find(|d| d.id == id) {
                        held.text.push_str(&text);
                        continue;
                    }
                    let Some(p) = self.live.get(&id) else { continue };
                    let part = if p.tool { PartKey::Input } else { PartKey::Body };
                    let (thread, item) = (p.thread.clone(), p.item.clone());
                    let thread = self.ensure(&thread);
                    self.push(thread, Action::Append { item, part, text });
                }
                Live::Clear { id } => self.cleared(&[Live::Clear { id }]),
            }
        }
        self.drain()
    }

    /// The agent's status, as the worker's tracker merged it from every signal.
    pub fn status(&mut self, event: &AgentEvent) -> Vec<Out> {
        if event.source == AgentSource::Hook {
            self.hear_hook();
        }
        let (phase, wait, liveness) = match &event.status {
            AgentStatus::None => (Phase::Idle, None, Liveness::Exited { resumable: true }),
            AgentStatus::Idle => (Phase::Idle, None, Liveness::Live),
            AgentStatus::Working | AgentStatus::Tool { .. } => {
                (Phase::Working, None, Liveness::Live)
            }
            AgentStatus::Done | AgentStatus::Blocked(BlockReason::IdlePrompt) => {
                (Phase::Done, None, Liveness::Live)
            }
            // The plan's limit is a wait of its own: the row's meters say when it lifts.
            AgentStatus::Failed { error, .. } if error == AgentStatus::RATE_LIMIT => {
                let wait = Wait { kind: Wait::LIMIT.to_owned(), text: LIMIT_TEXT.to_owned() };
                (Phase::Failed, Some(wait), Liveness::Live)
            }
            AgentStatus::Failed { .. } => (Phase::Failed, None, Liveness::Live),
            AgentStatus::Blocked(reason) => {
                let (kind, text) = match reason {
                    BlockReason::Permission { tool } => {
                        ("permission", permission_words(tool, event.detail.as_deref()))
                    }
                    BlockReason::Question => ("question", "Has a question".to_owned()),
                    BlockReason::Elicitation | BlockReason::IdlePrompt => {
                        ("input", "Needs input".to_owned())
                    }
                };
                // A permission is worded from the call; a question or an input is the agent's
                // own words where it gave any.
                let text = match reason {
                    BlockReason::Permission { .. } => text,
                    _ => event.detail.clone().unwrap_or(text),
                };
                (Phase::NeedsYou, Some(Wait { kind: kind.to_owned(), text }), Liveness::Live)
            }
            AgentStatus::Waiting { tasks, crons } => {
                let waiting = Waiting { tasks: *tasks, crons: *crons, named: event.detail.clone() };
                (Phase::Waiting, Some(self.wait_on(&waiting)), Liveness::Live)
            }
        };
        self.waiting_on = match event.status {
            AgentStatus::Waiting { tasks, crons } => {
                Some(Waiting { tasks, crons, named: event.detail.clone() })
            }
            _ => None,
        };
        let phase = if phase == Phase::Done && self.failed { Phase::Failed } else { phase };
        let status = Status { phase, wait, liveness, since_ms: event.since_ms };
        self.status = Some(status.clone());
        // Gone before a hook spoke, it asks nothing any more.
        if liveness != Liveness::Live
            && let Some(asked) = self.unheard.take()
        {
            let state = RequestState::Withdrawn;
            self.push(self.meta.id, Action::RequestResolved { id: asked.id, state });
        }
        if let Some(shown) = self.shown(Some(status.clone())) {
            self.push(self.meta.id, Action::Status(shown));
        }
        self.ask_in_terminal(event, &status);
        let mode = event.mode.as_ref().map(|m| m.name.clone());
        if mode.is_some() && mode != self.meters.mode {
            self.meters.mode = mode;
            self.push(self.meta.id, Action::MetersSet(self.meters.clone()));
        }
        self.drain()
    }

    /// What an agent at rest waits on: the tasks left running and the scheduled prompts, as
    /// the tracker counts them. When every one is a command the session's own transcript shows
    /// running in the background (a dev server, say), it waits on [`Wait::COMMAND`], named by
    /// those commands; a subagent, a monitor or a scheduled prompt among them is a task, named
    /// by the hook's words for a lone one, else counted.
    fn wait_on(&self, waiting: &Waiting) -> Wait {
        let (tasks, crons) = (waiting.tasks, waiting.crons);
        let running: Vec<&BackgroundTask> = self
            .threads
            .get(&conv::ThreadId::Main)
            .map(|m| m.tasks.iter().filter(|t| t.is_running()).collect())
            .unwrap_or_default();
        let only_commands = crons == 0
            && tasks > 0
            && usize::try_from(tasks).is_ok_and(|n| n == running.len())
            && running.iter().all(|t| t.kind == BackgroundTask::SHELL);
        if only_commands {
            let named: Vec<&str> = running.iter().map(|t| t.title.as_str()).collect();
            return Wait { kind: Wait::COMMAND.to_owned(), text: named.join(", ") };
        }
        let named = waiting.named.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let text = match (tasks, crons, named) {
            (1, 0, Some(named)) => named.to_owned(),
            (0, 1, _) => "1 scheduled prompt".to_owned(),
            (0, n, _) => format!("{n} scheduled prompts"),
            (1, ..) => "1 background task".to_owned(),
            (n, ..) => format!("{n} background tasks"),
        };
        Wait { kind: Wait::TASK.to_owned(), text }
    }

    /// Word the wait again once the transcript changed what runs in the background, while the
    /// agent waits at rest.
    fn rewait(&mut self) {
        let Some(waiting) = self.waiting_on.as_ref() else { return };
        let wait = self.wait_on(waiting);
        let Some(status) = self.status.as_mut().filter(|s| s.wait.as_ref() != Some(&wait)) else {
            return;
        };
        status.wait = Some(wait);
        let status = status.clone();
        self.push(self.meta.id, Action::Status(status));
    }

    /// The terminal's title changed, or was first heard. Claude Code paints the session's own
    /// name there (`✳ Fix the flaky test`, or the bare `Claude Code` before it has one), which
    /// names the thread until its first prompt does.
    pub fn title(&mut self, title: &str) -> Vec<Out> {
        if let Some(name) = session_name(title)
            && (self.meta.title.is_empty() || self.named)
            && self.meta.title != name
        {
            self.meta.title = name;
            self.named = true;
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// The terminal's working directory moved, or was first heard.
    pub fn cwd(&mut self, cwd: &str) -> Vec<Out> {
        if self.meta.cwd != cwd {
            cwd.clone_into(&mut self.meta.cwd);
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// The person's and the project's own commands in this session's folder
    /// (`crate::commands::custom`): the menu where the mod is not heard, and the argument hints
    /// of Claude Code's own list where it is.
    pub fn commands(&mut self, custom: &[conv::SlashCommand]) -> Vec<Out> {
        custom.clone_into(&mut self.custom);
        self.tell_commands();
        self.drain()
    }

    /// Claude Code's own lists, as the mod sent them: its slash commands become the menu
    /// ([`crate::commands::listed`]), and the aliases `/model` takes the thread's models, in
    /// place of [`MODELS`].
    pub fn catalog(&mut self, catalog: &live::Catalog) -> Vec<Out> {
        if self.catalog.as_ref() == Some(catalog) {
            return self.drain();
        }
        self.catalog = Some(catalog.clone());
        self.tell_commands();
        let models: Vec<Model> = catalog
            .models
            .iter()
            .map(|id| Model { id: id.clone(), label: alias_label(id) })
            .collect();
        if !models.is_empty() && models != self.meta.models {
            self.meta.models = models;
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// Tell the thread its menu, when it changed.
    fn tell_commands(&mut self) {
        let agent = self.catalog.as_ref().map(|c| c.commands.as_slice());
        let listed = crate::commands::listed(agent, &self.custom);
        let commands: Vec<thread::Command> = listed
            .iter()
            .map(|c| thread::Command {
                name: c.name.clone(),
                description: c.description.clone(),
                argument_hint: c.argument_hint.clone(),
                source: match c.source {
                    conv::CommandSource::BuiltIn => "built-in",
                    conv::CommandSource::Personal => "personal",
                    conv::CommandSource::Project => "project",
                    conv::CommandSource::Plugin => "plugin",
                }
                .to_owned(),
            })
            .collect();
        let reviews = commands.iter().any(|c| c.name == REVIEW_COMMAND);
        let review = Cap::named(Cap::REVIEW);
        let held = self.meta.caps.binary_search(&review);
        if reviews != held.is_ok() {
            match held {
                Ok(at) => drop(self.meta.caps.remove(at)),
                Err(at) => self.meta.caps.insert(at, review),
            }
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        if commands != self.commands {
            self.commands.clone_from(&commands);
            self.push(self.meta.id, Action::CommandsSet(commands));
        }
    }

    /// The status line's meters. A Claude Code thread shows no effort: Slopty does not follow
    /// it past the start, so the meters never claim one.
    pub fn meters(&mut self, meters: &conv::Meters) -> Vec<Out> {
        let limits = [("five-hour", meters.five_hour), ("seven-day", meters.seven_day)];
        let mapped = Meters {
            model: meters.model.clone(),
            model_id: meters.model_id.clone(),
            mode: self.meters.mode.clone(),
            effort: None,
            context_tokens: meters
                .context_used_pct
                .zip(meters.context_window)
                .map(|(pct, window)| share(window, pct)),
            context_window: meters.context_window,
            limits: limits
                .into_iter()
                .filter_map(|(name, window)| {
                    let window = window?;
                    Some(Limit {
                        name: name.to_owned(),
                        used_bp: u32::try_from(share(10_000, window.used_pct)).unwrap_or(u32::MAX),
                        resets_ms: window
                            .resets_at
                            .map(|s| WallMs::from_millis(s.saturating_mul(1000))),
                    })
                })
                .collect(),
        };
        if mapped != self.meters {
            self.meters = mapped;
            self.push(self.meta.id, Action::MetersSet(self.meters.clone()));
        }
        self.drain()
    }

    /// A permission prompt held for the session, or settled.
    pub fn permission(&mut self, event: &PermissionEvent) -> Vec<Out> {
        let action = match event {
            PermissionEvent::Asked(prompt) => {
                self.hear_hook();
                self.held_block = self.blocked_since();
                // The prompt is held here after all: it is answered on its own card.
                if let Some(asked) = self.in_terminal.take() {
                    let id = asked.id;
                    self.push(
                        self.meta.id,
                        Action::RequestResolved { id, state: RequestState::Withdrawn },
                    );
                }
                let request = request(prompt);
                self.open.push(request.clone());
                if let Some(call) = &request.item {
                    self.call_state(call, &ToolState::Pending { ask: request.id.clone() });
                }
                Action::RequestOpened(Box::new(request))
            }
            PermissionEvent::Settled { ask, outcome, .. } => {
                self.held_block = self.held_block.or_else(|| self.blocked_since());
                let id = ask.to_string();
                let calls: Vec<ItemId> = self
                    .open
                    .iter()
                    .filter(|r| r.id.0 == id)
                    .filter_map(|r| r.item.clone())
                    .collect();
                self.open.retain(|r| r.id.0 != id);
                for call in &calls {
                    self.call_state(call, &ToolState::Running);
                }
                let state = match outcome {
                    Settled::Answered { verdict, by } => {
                        RequestState::Answered { by: answerer(*by), choice: choice_of(verdict) }
                    }
                    Settled::Released => RequestState::Released,
                    Settled::Withdrawn => RequestState::Withdrawn,
                };
                Action::RequestResolved { id: AskId(id), state }
            }
            PermissionEvent::Declined(declined) => {
                self.declined(declined);
                return self.drain();
            }
        };
        self.push(self.meta.id, action);
        self.drain()
    }

    /// Set the state of `call`, a call not ended yet, and send it again: a prompt held for it
    /// marks it waiting on the person, and its settling lets it run. A call not in the
    /// transcript yet, or ended, is left as it is.
    fn call_state(&mut self, call: &ItemId, state: &ToolState) {
        let found = self.threads.values_mut().find_map(|m| {
            let item = m.calls.get_mut(&call.0)?;
            let ItemBody::Tool(tool) = &mut item.body else { return None };
            if tool.state == *state {
                return None;
            }
            tool.state = state.clone();
            Some((m.id, item.clone()))
        });
        if let Some((thread, item)) = found {
            self.push(thread, Action::ItemUpdated(item));
        }
    }

    /// Auto mode turned a call down: say so after the call, or once the transcript has it.
    fn declined(&mut self, declined: &conv::Declined) {
        let notice = decline_notice(declined);
        match &declined.call {
            Some(call) if !self.threads.values().any(|m| m.items.contains_key(call)) => {
                self.declines.insert(call.clone(), (notice, declined.at_ms));
            }
            call => self.say_declined(call.as_deref(), notice, declined.at_ms),
        }
    }

    /// The notice of a decline, after call `call` in its thread and turn; with no call named,
    /// at the end of the main thread's turn. Never before the first prompt.
    fn say_declined(&mut self, call: Option<&str>, notice: Notice, at_ms: WallMs) {
        let placed = call.and_then(|call| {
            self.threads.iter().find_map(|(thread, m)| Some((thread.clone(), m.items.get(call)?.0)))
        });
        let main = || {
            let turn = self.threads.get(&conv::ThreadId::Main).map_or(TurnId::BEFORE, |m| m.turn);
            (conv::ThreadId::Main, turn)
        };
        let (thread, turn) = placed.unwrap_or_else(main);
        if turn == TurnId::BEFORE {
            return;
        }
        let id = self.ensure(&thread);
        let named = call.map_or_else(|| at_ms.as_millis().to_string(), str::to_owned);
        let item = Item {
            id: ItemId(format!("declined-{named}")),
            turn,
            at_ms,
            body: ItemBody::Notice(notice),
        };
        self.push(id, Action::ItemCompleted(item));
    }

    /// When the block the thread is in began, while it is blocked on the person.
    fn blocked_since(&self) -> Option<WallMs> {
        self.status.as_ref().filter(|s| s.phase == Phase::NeedsYou).map(|s| s.since_ms)
    }

    /// Note what the agent's status `event` (mapped to `status`) asks in its own terminal, and
    /// settle the request made for an earlier block once this status ends it: answered there
    /// when the agent goes back to work, else withdrawn. The request itself opens only after
    /// [`ASK_GRACE`], from [`Observed::waited`].
    fn ask_in_terminal(&mut self, event: &AgentEvent, status: &Status) {
        self.blocked_kind = match &event.status {
            AgentStatus::Blocked(BlockReason::Permission { .. }) => Some(Request::APPROVAL),
            AgentStatus::Blocked(BlockReason::Question) => Some(Request::QUESTION),
            AgentStatus::Blocked(BlockReason::Elicitation) => Some(Request::ELICITATION),
            _ => None,
        };
        let Some(mut asked) = self.in_terminal.take() else { return };
        if let (Some(kind), Some(wait)) = (self.blocked_kind, status.wait.as_ref())
            && asked.id == terminal_ask(status.since_ms)
        {
            // The same block: told again only if what it asks moved.
            if asked.kind != kind || asked.title != wait.text {
                kind.clone_into(&mut asked.kind);
                asked.title.clone_from(&wait.text);
                self.push(self.meta.id, Action::RequestOpened(Box::new(asked.clone())));
            }
            self.in_terminal = Some(asked);
            return;
        }
        let state = if status.phase == Phase::Working {
            RequestState::Answered {
                by: Answerer { client: None, name: IN_TERMINAL.to_owned() },
                choice: String::new(),
            }
        } else {
            RequestState::Withdrawn
        };
        self.push(self.meta.id, Action::RequestResolved { id: asked.id, state });
    }

    /// Time has come to `now`: a block that still asks with no prompt held here, for
    /// [`ASK_GRACE`] since it began, opens its request, answered only in the agent's own
    /// terminal. The grace lets a prompt held for the block come first, so a held one never
    /// flashes a card it then replaces.
    pub fn waited(&mut self, now: WallMs) -> Vec<Out> {
        let Some(kind) = self.blocked_kind else { return self.drain() };
        let Some(status) = self.status.as_ref() else { return self.drain() };
        let since = status.since_ms;
        let grace = u64::try_from(ASK_GRACE.as_millis()).unwrap_or(u64::MAX);
        let due = now.as_millis() >= since.as_millis().saturating_add(grace);
        let asks = self.in_terminal.is_none()
            && self.open.is_empty()
            && self.meta.terminal.is_some()
            && self.held_block != Some(since)
            && due;
        if let (true, Some(wait)) = (asks, status.wait.as_ref()) {
            let request = Request {
                id: terminal_ask(since),
                item: None,
                kind: kind.to_owned(),
                title: wait.text.clone(),
                text: None,
                options: Vec::new(),
                questions: Vec::new(),
                proposed: None,
                schema_json: None,
                url: None,
                state: RequestState::Open,
                opened_ms: since,
                until_ms: None,
            };
            self.in_terminal = Some(request.clone());
            self.push(self.meta.id, Action::RequestOpened(Box::new(request)));
        }
        self.drain()
    }

    fn push(&mut self, thread: ThreadId, action: Action) {
        if let Some(Out::Actions(last, actions)) = self.out.last_mut()
            && *last == thread
        {
            actions.push(action);
            return;
        }
        self.out.push(Out::Actions(thread, vec![action]));
    }

    /// The thread the decoder's `thread` maps to, begun the first time it is seen.
    fn ensure(&mut self, thread: &conv::ThreadId) -> ThreadId {
        if let Some(mapped) = self.threads.get(thread) {
            return mapped.id;
        }
        let agent = match thread {
            conv::ThreadId::Agent(agent) => agent.clone(),
            conv::ThreadId::Main => return self.meta.id,
        };
        let id = self.meta.id.subagent(&agent);
        self.out.push(Out::Begin(Box::new(self.subagent_meta(id, &agent))));
        self.threads.insert(thread.clone(), Mapped::new(id));
        id
    }

    fn subagent_meta(&self, id: ThreadId, agent: &str) -> ThreadMeta {
        let parent = self.spawned.get(agent).map(|(thread, call)| Link {
            thread: self.threads.get(thread).map_or(self.meta.id, |m| m.id),
            item: ItemId(call.clone()),
        });
        ThreadMeta {
            id,
            native: agent.to_owned(),
            title: String::new(),
            parent,
            origin: ThreadMeta::SUBAGENT.to_owned(),
            caps: Vec::new(),
            models: Vec::new(),
            ..self.meta.clone()
        }
    }

    fn reset(&mut self, thread: Option<&conv::ThreadId>) {
        let gone: Vec<conv::ThreadId> = match thread {
            Some(thread) => vec![thread.clone()],
            None => self.threads.keys().cloned().collect(),
        };
        for thread in gone {
            let Some(mapped) = self.threads.get_mut(&thread) else { continue };
            let id = mapped.id;
            *mapped = Mapped::new(id);
            let meta = match &thread {
                conv::ThreadId::Main => self.meta.clone(),
                conv::ThreadId::Agent(agent) => self.subagent_meta(id, agent),
            };
            self.out.push(Out::Begin(Box::new(meta)));
            if thread == conv::ThreadId::Main {
                self.failed = false;
                self.again();
            }
        }
        self.live.clear();
        self.deferred.clear();
        let _shown = self.overlay.clear_all();
    }

    /// What the main thread holds that its transcript does not: told again after it began
    /// anew.
    fn again(&mut self) {
        let mut actions: Vec<Action> =
            self.shown(self.status.clone()).into_iter().map(Action::Status).collect();
        if self.meters != Meters::default() {
            actions.push(Action::MetersSet(self.meters.clone()));
        }
        actions.extend(
            self.open
                .iter()
                .chain(&self.in_terminal)
                .chain(&self.unheard)
                .cloned()
                .map(|r| Action::RequestOpened(Box::new(r))),
        );
        if !self.commands.is_empty() {
            actions.push(Action::CommandsSet(self.commands.clone()));
        }
        for action in actions {
            self.push(self.meta.id, action);
        }
    }

    fn entry(&mut self, thread: &conv::ThreadId, entry: &conv::Entry) {
        if let Body::Tool(call) = &entry.body
            && let conv::ToolDetail::Agent(agent) = &call.detail
            && let Some(agent_id) = &agent.agent_id
        {
            self.spawned.insert(agent_id.clone(), (thread.clone(), entry.id.clone()));
        }
        let id = self.ensure(thread);
        let body = self.body(thread, entry);
        if id == self.meta.id
            && (self.meta.title.is_empty() || self.named)
            && let Body::Prompt(prompt) = &entry.body
            && prompt.command.is_none()
        {
            self.meta.title = title_of(&prompt.text.text);
            self.named = false;
            self.push(id, Action::Meta(Box::new(self.meta.clone())));
        }
        let Some(mapped) = self.threads.get_mut(thread) else { return };
        let mut actions = Vec::new();
        let (mut began, mut opened) = (false, false);
        let turn = match (&entry.body, mapped.items.get(&entry.id)) {
            (_, Some((turn, _))) => *turn,
            (Body::Prompt(_), None) => {
                let turn = mapped.turn.next();
                mapped.turn = turn;
                mapped.prompts.insert(entry.id.clone(), turn);
                let started = Turn {
                    id: turn,
                    input: Some(ItemId(entry.id.clone())),
                    state: TurnState::Active,
                    started_ms: entry.at_ms,
                    ended_ms: None,
                    usage: Usage::default(),
                    models: Vec::new(),
                    changed: Changed::default(),
                    before: None,
                    after: None,
                };
                mapped.turns.insert(turn, started.clone());
                actions.push(Action::TurnStarted(started));
                began = *thread == conv::ThreadId::Main;
                opened = true;
                turn
            }
            (_, None) => mapped.turn,
        };
        match &entry.body {
            Body::Interrupted { .. } => {
                mapped.interrupted.insert(turn);
            }
            Body::Note(conv::Note { stop: Some(stop), text, .. }) => {
                mapped.stopped.insert(turn, (stop.clone(), text.text.clone()));
            }
            Body::Text(_) | Body::Thinking(_) | Body::Tool(_) => {
                mapped.stopped.remove(&turn);
            }
            _ => {}
        }
        let changed = changed_by(&entry.body);
        let had = mapped.items.insert(entry.id.clone(), (turn, changed)).map(|(_, c)| c);
        let mut item = Item { id: ItemId(entry.id.clone()), turn, at_ms: entry.at_ms, body };
        if let ItemBody::Tool(call) = &mut item.body
            && call.state == ToolState::Running
            && let Some(asked) = self.open.iter().find(|r| r.item.as_ref() == Some(&item.id))
        {
            call.state = ToolState::Pending { ask: asked.id.clone() };
        }
        let background = matches!(&item.body, ItemBody::Tool(call)
            if matches!(&call.detail, Some(detail::ToolDetail::Exec(e)) if e.background));
        if background {
            mapped.background.insert(entry.id.clone(), item.clone());
        }
        let task = task_of(entry, &item);
        if mapped.set_task(&entry.id, task) {
            actions.push(Action::TasksSet(mapped.tasks.clone()));
        }
        let open = matches!(&item.body, ItemBody::Tool(call) if !call.state.is_final());
        if open {
            mapped.calls.insert(entry.id.clone(), item.clone());
        } else {
            mapped.calls.remove(&entry.id);
        }
        actions.push(if open { Action::ItemUpdated(item) } else { Action::ItemCompleted(item) });
        if had.unwrap_or_default() != changed
            && let Some(mut t) = mapped.turns.get(&turn).cloned()
        {
            t.changed = mapped.changed(turn);
            mapped.turns.insert(turn, t.clone());
            actions.push(Action::TurnStarted(t));
        }
        for action in actions {
            self.push(id, action);
        }
        if let Some((notice, at_ms)) = self.declines.remove(&entry.id) {
            self.say_declined(Some(&entry.id), notice, at_ms);
        }
        if opened {
            self.place_deferred(thread);
        }
        self.rewait();
        // A new prompt leaves the failure behind: its own turn's end says how it went.
        if began {
            self.failed = false;
        }
    }

    /// `thread`'s prompt opened its turn: the live blocks held for it begin there, after it,
    /// with what they said meanwhile.
    fn place_deferred(&mut self, thread: &conv::ThreadId) {
        let (held, kept): (Vec<Deferred>, Vec<Deferred>) =
            std::mem::take(&mut self.deferred).into_iter().partition(|d| d.thread == *thread);
        self.deferred = kept;
        for Deferred { thread, id, kind, text } in held {
            self.live_start(&thread, id.clone(), &kind);
            let Some(p) = self.live.get(&id).filter(|_| !text.is_empty()) else { continue };
            let part = if p.tool { PartKey::Input } else { PartKey::Body };
            let item = p.item.clone();
            let thread = self.ensure(&thread);
            self.push(thread, Action::Append { item, part, text });
        }
    }

    fn remove(&mut self, thread: &conv::ThreadId, entry: &str) {
        let id = self.ensure(thread);
        let Some(mapped) = self.threads.get_mut(thread) else { return };
        let mut actions = vec![Action::ItemRemoved { item: ItemId(entry.to_owned()) }];
        mapped.items.remove(entry);
        mapped.background.remove(entry);
        mapped.calls.remove(entry);
        if mapped.set_task(entry, None) {
            actions.push(Action::TasksSet(mapped.tasks.clone()));
        }
        if let Some(turn) = mapped.prompts.remove(entry) {
            let after = TurnId(turn.0.saturating_sub(1));
            mapped.turn = after;
            mapped.prompts.retain(|_, t| *t < turn);
            mapped.items.retain(|_, (t, _)| *t < turn);
            mapped.turns.retain(|t, _| *t < turn);
            mapped.ended.retain(|t| *t < turn);
            mapped.interrupted.retain(|t| *t < turn);
            mapped.stopped.retain(|t, _| *t < turn);
            actions.push(Action::Truncated { after: Some(after) });
        }
        for action in actions {
            self.push(id, action);
        }
        self.rewait();
    }

    fn turn(&mut self, thread: &conv::ThreadId, record: &conv::Turn) {
        let id = self.ensure(thread);
        // A turn the person began after Slopty opened Claude Code is past any dialog it was
        // held at, though no hook spoke: hooks the person's managed settings keep off.
        let opened = self.unheard.as_ref().map(|asked| {
            let unheard = u64::try_from(UNHEARD.as_millis()).unwrap_or(u64::MAX);
            asked.opened_ms.as_millis().saturating_sub(unheard)
        });
        if id == self.meta.id && opened.is_some_and(|at| record.started_ms.as_millis() >= at) {
            self.answered_in_terminal();
        }
        let Some(mapped) = self.threads.get_mut(thread) else { return };
        let Some(&turn) = mapped.prompts.get(&record.prompt) else { return };
        let state = match (record.ended_ms, mapped.stopped.get(&turn)) {
            (None, _) => TurnState::Active,
            (Some(_), _) if mapped.interrupted.contains(&turn) => TurnState::Interrupted,
            (Some(_), Some((stop, error))) => TurnState::Failed {
                error: error.clone(),
                until_ms: stop.until_ms.or_else(|| {
                    stop.limited()
                        .then(|| crate::driven::reset_of_full(&self.meters.limits))
                        .flatten()
                }),
            },
            (Some(_), None) => TurnState::Complete,
        };
        let usage = usage(&record.usage);
        let mut updated = mapped.turns.get(&turn).cloned().unwrap_or_else(|| Turn {
            id: turn,
            input: Some(ItemId(record.prompt.clone())),
            state: TurnState::Active,
            started_ms: record.started_ms,
            ended_ms: None,
            usage: Usage::default(),
            models: Vec::new(),
            changed: Changed::default(),
            before: None,
            after: None,
        });
        updated.models.clone_from(&record.models);
        updated.usage = usage.clone();
        updated.changed = mapped.changed(turn);
        if let Some(ended_ms) = record.ended_ms {
            updated.state = state.clone();
            updated.ended_ms = Some(ended_ms);
        }
        let mut actions = Vec::new();
        if mapped.turns.get(&turn) != Some(&updated) {
            mapped.turns.insert(turn, updated.clone());
            actions.push(Action::TurnStarted(updated));
        }
        let mut failed = None;
        if let Some(ended_ms) = record.ended_ms
            && mapped.ended.insert(turn)
        {
            failed = Some(matches!(state, TurnState::Failed { .. }));
            actions.push(Action::TurnEnded { turn, state, usage, ended_ms });
        }
        for action in actions {
            self.push(id, action);
        }
        if *thread == conv::ThreadId::Main
            && let Some(failed) = failed
        {
            self.failed = failed;
            self.settle_failed();
        }
    }

    /// A status of done after a turn that failed is a failure: tell it again as one.
    fn settle_failed(&mut self) {
        let Some(status) = self.status.as_mut() else { return };
        let phase = match status.phase {
            Phase::Done if self.failed => Phase::Failed,
            Phase::Failed if !self.failed => Phase::Done,
            _ => return,
        };
        status.phase = phase;
        let status = status.clone();
        self.push(self.meta.id, Action::Status(status));
    }

    fn output(&mut self, output: &conv::Output) {
        let id = self.ensure(&output.thread);
        let Some(mapped) = self.threads.get_mut(&output.thread) else { return };
        let Some(item) = mapped.background.get_mut(&output.call) else { return };
        let tail = clipped(&output.thread, &output.tail);
        if let ItemBody::Tool(call) = &mut item.body {
            call.output = Some(tail.clone());
        }
        let item = item.clone();
        let tasks = if let Some(task) =
            mapped.tasks.iter_mut().find(|t| t.item.as_ref() == Some(&item.id))
            && task.output.as_ref() != Some(&tail)
        {
            task.output = Some(tail);
            Some(mapped.tasks.clone())
        } else {
            None
        };
        self.push(id, Action::ItemUpdated(item));
        if let Some(tasks) = tasks {
            self.push(id, Action::TasksSet(tasks));
        }
    }

    /// A live block begins: an item in its thread's open turn. With no turn open (the last one
    /// ended, or none began), the block's prompt is not in the transcript yet, and an item now
    /// would stand ahead of it: the block waits for the prompt ([`Deferred`]).
    fn live_start(&mut self, thread: &conv::ThreadId, id: LiveId, kind: &LiveKind) {
        let thread_id = self.ensure(thread);
        let Some(mapped) = self.threads.get(thread) else { return };
        let turn = mapped.turn;
        if turn == TurnId::BEFORE || mapped.ended.contains(&turn) {
            let (thread, kind) = (thread.clone(), kind.clone());
            self.deferred.push(Deferred { thread, id, kind, text: String::new() });
            return;
        }
        let (item, body, tool) = match kind {
            LiveKind::Text => (live_item(&id), ItemBody::Text(Clipped::default()), false),
            LiveKind::Thinking => (live_item(&id), ItemBody::Reasoning(Clipped::default()), false),
            LiveKind::Tool { id: call, name } => {
                if mapped.items.contains_key(call) {
                    return;
                }
                let body = ItemBody::Tool(Box::new(ToolCall {
                    name: name.clone(),
                    kind: kind_of_name(name).to_owned(),
                    title: name.clone(),
                    input: Clipped::default(),
                    state: ToolState::Streaming,
                    output: None,
                    images: Vec::new(),
                    detail: None,
                    child: None,
                    ended_ms: None,
                }));
                (ItemId(call.clone()), body, true)
            }
        };
        self.live.insert(id, Provisional { thread: thread.clone(), item: item.clone(), tool });
        let item = Item { id: item, turn, at_ms: WallMs::ZERO, body };
        self.push(thread_id, Action::ItemStarted(item));
    }

    /// Live blocks let go: each item goes, but a tool call's the transcript has by now,
    /// which is the transcript's own.
    fn cleared(&mut self, cleared: &[Live]) {
        for change in cleared {
            let Live::Clear { id } = change else { continue };
            self.deferred.retain(|d| d.id != *id);
            let Some(p) = self.live.remove(id) else { continue };
            let thread = self.ensure(&p.thread);
            let settled =
                self.threads.get(&p.thread).is_some_and(|m| m.items.contains_key(&p.item.0));
            if !(p.tool && settled) {
                self.push(thread, Action::ItemRemoved { item: p.item });
            }
        }
    }

    fn body(&self, thread: &conv::ThreadId, entry: &conv::Entry) -> ItemBody {
        let clip = |c: &conv::Clipped| clipped(thread, c);
        match &entry.body {
            Body::Prompt(prompt) => ItemBody::User(UserMessage {
                text: clip(&prompt.text),
                images: prompt.images.iter().map(|i| image(thread, i)).collect(),
                command: prompt.command.clone(),
                intent: None,
            }),
            Body::Text(text) => ItemBody::Text(clip(text)),
            Body::Thinking(text) => ItemBody::Reasoning(clip(text)),
            Body::Tool(call) => ItemBody::Tool(Box::new(self.tool(thread, call))),
            Body::Compact(compact) => ItemBody::Compaction(Compaction {
                trigger: compact.trigger.clone(),
                before_tokens: compact.pre_tokens,
                after_tokens: compact.post_tokens,
                summary: compact.summary.as_ref().map(clip),
            }),
            Body::Interrupted { during_tool } => ItemBody::Notice(Notice::new(
                Notice::INTERRUPTED,
                Clipped::whole(if *during_tool {
                    "Interrupted during a tool call"
                } else {
                    "Interrupted"
                }),
            )),
            Body::Note(note) => ItemBody::Notice(Notice {
                kind: match note.kind {
                    NoteKind::ApiError if note.stop.as_ref().is_some_and(conv::Stop::limited) => {
                        Notice::LIMIT
                    }
                    NoteKind::ApiError => Notice::API_ERROR,
                    NoteKind::Command => Notice::COMMAND,
                    NoteKind::Info => Notice::INFO,
                    NoteKind::Hook => Notice::HOOK,
                }
                .to_owned(),
                text: clip(&note.text),
                retry: note.retry.map(|r| Retry {
                    attempt: r.attempt,
                    max: Some(r.max),
                    in_ms: Some(r.in_ms),
                }),
            }),
            Body::Rewound { dropped } => ItemBody::Notice(Notice::new(
                Notice::REWOUND,
                Clipped::whole(&format!(
                    "Went back to an earlier message; {dropped} later entries left"
                )),
            )),
        }
    }

    fn tool(&self, thread: &conv::ThreadId, call: &conv::ToolCall) -> ToolCall {
        let clip = |c: &conv::Clipped| clipped(thread, c);
        let state = match call.result.as_ref().map(|r| r.status) {
            None => ToolState::Running,
            Some(ResultStatus::Ok) => ToolState::Completed,
            Some(ResultStatus::Error) => ToolState::Failed,
            Some(ResultStatus::Rejected) => ToolState::Rejected,
        };
        let mut output = call.result.as_ref().and_then(|r| r.text.as_ref()).map(clip);
        let images = call.result.iter().flat_map(|r| &r.images).map(|i| image(thread, i)).collect();
        let mut input = Clipped::default();
        let mut child = None;
        let (kind, title, detail) = match &call.detail {
            conv::ToolDetail::Edit(e) => {
                (kind::EDIT, format!("Edit {}", e.path), Some(detail::ToolDetail::Edit(e.clone())))
            }
            conv::ToolDetail::Write(w) => (
                kind::WRITE,
                format!("Write {}", w.path),
                Some(detail::ToolDetail::Write(w.clone())),
            ),
            conv::ToolDetail::Read(r) => (
                kind::READ,
                format!("Read {}", r.path),
                Some(detail::ToolDetail::Read(detail::ReadDetail {
                    path: r.path.clone(),
                    offset: r.offset,
                    limit: r.limit,
                    lines: r.lines,
                    total_lines: r.total_lines,
                })),
            ),
            conv::ToolDetail::Grep(g) => (
                kind::SEARCH,
                format!("Search for {}", g.pattern),
                Some(detail::ToolDetail::Search(detail::SearchDetail {
                    pattern: g.pattern.clone(),
                    path: g.path.clone(),
                    glob: g.glob.clone(),
                    files: g.files,
                    matches: g.lines,
                    truncated: false,
                })),
            ),
            conv::ToolDetail::Glob(g) => (
                kind::SEARCH,
                format!("Find {}", g.pattern),
                Some(detail::ToolDetail::Search(detail::SearchDetail {
                    pattern: g.pattern.clone(),
                    path: g.path.clone(),
                    glob: None,
                    files: g.files,
                    matches: None,
                    truncated: g.truncated,
                })),
            ),
            conv::ToolDetail::Bash(b) => {
                if let Some(stdout) = &b.stdout {
                    output = Some(clip(stdout));
                }
                let first = b.command.text.lines().next().unwrap_or_default();
                (
                    kind::EXEC,
                    b.description.clone().unwrap_or_else(|| format!("Run {first}")),
                    Some(detail::ToolDetail::Exec(detail::ExecDetail {
                        command: clip(&b.command),
                        description: b.description.clone(),
                        cwd: None,
                        background: b.background,
                        task: b.task_id.clone(),
                        status: match b.status {
                            conv::ShellStatus::Running => detail::ExecStatus::Running,
                            conv::ShellStatus::Done => detail::ExecStatus::Done,
                            conv::ShellStatus::Failed => detail::ExecStatus::Failed,
                            conv::ShellStatus::Interrupted | conv::ShellStatus::Killed => {
                                detail::ExecStatus::Interrupted
                            }
                        },
                        exit_code: b.exit_code,
                        stderr: b.stderr.as_ref().map(clip),
                        duration_ms: None,
                    })),
                )
            }
            conv::ToolDetail::WebFetch(f) => (
                kind::FETCH,
                format!("Fetch {}", f.url),
                Some(detail::ToolDetail::Fetch(detail::FetchDetail {
                    url: f.url.clone(),
                    prompt: f.prompt.clone(),
                    code: f.code,
                    bytes: f.bytes,
                })),
            ),
            conv::ToolDetail::WebSearch(s) => (
                kind::WEB_SEARCH,
                format!("Search the web for {}", s.query),
                Some(detail::ToolDetail::WebSearch(detail::WebSearchDetail {
                    query: s.query.clone(),
                    results: s.results,
                    links: s
                        .links
                        .iter()
                        .map(|l| detail::WebLink { title: l.title.clone(), url: l.url.clone() })
                        .collect(),
                })),
            ),
            conv::ToolDetail::Agent(a) => {
                child = a.agent_id.as_ref().map(|agent| self.meta.id.subagent(agent));
                (
                    kind::AGENT,
                    a.description.clone().unwrap_or_else(|| "Run a subagent".to_owned()),
                    Some(detail::ToolDetail::Agent(detail::AgentDetail {
                        agent_type: a.agent_type.clone(),
                        description: a.description.clone(),
                        prompt: clip(&a.prompt),
                        background: a.background,
                        report: a.report.as_ref().map(clip),
                        tokens: a.tokens,
                        tool_uses: a.tool_uses,
                        duration_ms: a.duration_ms,
                    })),
                )
            }
            conv::ToolDetail::TaskCreate(t) => (
                kind::TASKS,
                format!("Add a task: {}", t.subject),
                Some(detail::ToolDetail::Tasks {
                    steps: vec![Step {
                        id: t.task_id.clone(),
                        text: t.subject.clone(),
                        status: "pending".to_owned(),
                    }],
                    whole: false,
                }),
            ),
            conv::ToolDetail::TaskUpdate(t) => (
                kind::TASKS,
                t.subject
                    .clone()
                    .map_or_else(|| "Update a task".to_owned(), |s| format!("Update a task: {s}")),
                Some(detail::ToolDetail::Tasks {
                    steps: vec![Step {
                        id: Some(t.task_id.clone()),
                        text: t.subject.clone().unwrap_or_default(),
                        status: t.to.clone().unwrap_or_default(),
                    }],
                    whole: false,
                }),
            ),
            conv::ToolDetail::TodoWrite { todos } => (
                kind::TASKS,
                "Update the task list".to_owned(),
                Some(detail::ToolDetail::Tasks {
                    steps: todos.iter().map(step).collect(),
                    whole: true,
                }),
            ),
            conv::ToolDetail::Question(q) => (
                kind::QUESTION,
                q.questions.first().map_or_else(|| "Ask a question".to_owned(), |q| q.text.clone()),
                Some(detail::ToolDetail::Question(question(q))),
            ),
            conv::ToolDetail::Plan { plan } => (
                kind::PLAN,
                "Propose a plan".to_owned(),
                Some(detail::ToolDetail::Plan { text: clip(plan) }),
            ),
            conv::ToolDetail::Mcp(m) => {
                input = clip(&m.input);
                (
                    kind::MCP,
                    format!("{}: {}", m.server, m.tool),
                    Some(detail::ToolDetail::Mcp(detail::McpDetail {
                        server: m.server.clone(),
                        tool: m.tool.clone(),
                    })),
                )
            }
            conv::ToolDetail::Other { input: raw } => {
                input = clip(raw);
                (kind::OTHER, call.name.clone(), None)
            }
        };
        ToolCall {
            name: call.name.clone(),
            kind: kind.to_owned(),
            title,
            input,
            state,
            output,
            images,
            detail,
            child,
            ended_ms: call.result.as_ref().map(|r| r.at_ms),
        }
    }
}

fn live_item(id: &LiveId) -> ItemId {
    ItemId(format!("live:{}:{}:{}", id.turn, id.step, id.block))
}

/// A tool's kind from its name alone, for a call the mod reports before the transcript types
/// it.
fn kind_of_name(name: &str) -> &'static str {
    match name {
        "Edit" | "MultiEdit" | "NotebookEdit" => kind::EDIT,
        "Write" => kind::WRITE,
        "Read" => kind::READ,
        "Grep" | "Glob" => kind::SEARCH,
        "Bash" => kind::EXEC,
        "WebFetch" => kind::FETCH,
        "WebSearch" => kind::WEB_SEARCH,
        "Agent" | "Task" => kind::AGENT,
        "AskUserQuestion" => kind::QUESTION,
        "ExitPlanMode" => kind::PLAN,
        "TaskCreate" | "TaskUpdate" | "TodoWrite" => kind::TASKS,
        name if name.starts_with("mcp__") => kind::MCP,
        _ => kind::OTHER,
    }
}

/// The work `entry` left running in the background, as its mapped `item` says it: a command
/// run in the background, by its task id once Claude Code gave one, or a subagent run in the
/// background. Neither is one once it was asked to run in the foreground.
fn task_of(entry: &conv::Entry, item: &Item) -> Option<BackgroundTask> {
    let Body::Tool(call) = &entry.body else { return None };
    let ItemBody::Tool(mapped) = &item.body else { return None };
    let (id, kind, state, ended_ms) = match &call.detail {
        conv::ToolDetail::Bash(b) if b.background => {
            let state = match b.status {
                conv::ShellStatus::Running => BackgroundTask::RUNNING,
                conv::ShellStatus::Done => BackgroundTask::COMPLETED,
                conv::ShellStatus::Failed => BackgroundTask::FAILED,
                conv::ShellStatus::Interrupted | conv::ShellStatus::Killed => {
                    BackgroundTask::KILLED
                }
            };
            let id = b.task_id.clone().unwrap_or_else(|| entry.id.clone());
            (id, BackgroundTask::SHELL, state, b.finished_ms)
        }
        conv::ToolDetail::Agent(a) if a.background => {
            let state = match a.status {
                conv::AgentRun::Running => BackgroundTask::RUNNING,
                conv::AgentRun::Completed => BackgroundTask::COMPLETED,
                conv::AgentRun::Failed => BackgroundTask::FAILED,
                conv::AgentRun::Killed => BackgroundTask::KILLED,
            };
            let ended = a
                .duration_ms
                .filter(|_| state != BackgroundTask::RUNNING)
                .map(|ms| WallMs::from_millis(entry.at_ms.as_millis().saturating_add(ms)));
            let id = a.agent_id.clone().unwrap_or_else(|| entry.id.clone());
            (id, BackgroundTask::AGENT, state, ended)
        }
        _ => return None,
    };
    Some(BackgroundTask {
        id,
        kind: kind.to_owned(),
        title: mapped.title.clone(),
        state: state.to_owned(),
        item: Some(item.id.clone()),
        output: mapped.output.clone().filter(|_| kind == BackgroundTask::SHELL),
        started_ms: entry.at_ms,
        ended_ms,
    })
}

/// The lines an entry changed: an edit's or a write's diff.
fn changed_by(body: &Body) -> Changed {
    let Body::Tool(call) = body else { return Changed::default() };
    let patch = match &call.detail {
        conv::ToolDetail::Edit(e) => &e.patch,
        conv::ToolDetail::Write(w) => &w.patch,
        _ => return Changed::default(),
    };
    Changed { added: patch.added, removed: patch.removed }
}

/// The session's own name in a Claude Code terminal title: what follows its glyph, unless
/// that is only the program's name.
fn session_name(title: &str) -> Option<String> {
    let title = title.trim();
    let glyph = title.chars().next()?;
    if glyph != crate::title::IDLE && !crate::title::WORKING.contains(&glyph) {
        return None;
    }
    let name = title.get(glyph.len_utf8()..)?.trim();
    (!name.is_empty() && !name.eq_ignore_ascii_case("claude code")).then(|| title_of(name))
}

/// A thread's title from its first prompt: the first line, cut at a word.
fn title_of(prompt: &str) -> String {
    const MAX: usize = 80;
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    if line.chars().count() <= MAX {
        return line.to_owned();
    }
    let cut: String = line.chars().take(MAX).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{cut}…")
}

/// `whole × pct / 100`, rounded down.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a share of a token count or a dollar figure, clamped at zero; a float holds it"
)]
fn share(whole: u64, pct: f64) -> u64 {
    (whole as f64 * pct.max(0.0) / 100.0) as u64
}

fn usage(usage: &conv::Usage) -> Usage {
    let counts = [
        (Usage::INPUT, usage.input),
        (Usage::CACHE_READ, usage.cache_read),
        (Usage::CACHE_WRITE, usage.cache_write),
        (Usage::OUTPUT, usage.output),
        (Usage::REASONING, usage.thinking),
    ];
    Usage(counts.into_iter().filter(|(_, n)| *n > 0).map(|(k, n)| (k.to_owned(), n)).collect())
}

fn step(task: &conv::Task) -> Step {
    Step { id: Some(task.id.clone()), text: task.subject.clone(), status: task.status.clone() }
}

fn clipped(thread: &conv::ThreadId, c: &conv::Clipped) -> Clipped {
    Clipped {
        text: c.text.clone(),
        lines: c.lines,
        chars: c.chars,
        full: c.full.as_ref().map(|r| content_ref(thread, r)),
    }
}

fn image(thread: &conv::ThreadId, i: &conv::Image) -> thread::Image {
    thread::Image {
        digest: i.digest.clone(),
        media_type: i.media_type.clone(),
        bytes: i.bytes,
        width: i.width,
        height: i.height,
        at: content_ref(thread, &i.at),
    }
}

fn question(q: &conv::QuestionDetail) -> detail::QuestionDetail {
    detail::QuestionDetail {
        questions: q
            .questions
            .iter()
            .map(|q| detail::Question {
                text: q.text.clone(),
                header: q.header.clone(),
                options: q
                    .options
                    .iter()
                    .map(|o| detail::Offered {
                        label: o.label.clone(),
                        description: o.description.clone(),
                    })
                    .collect(),
                multi_select: q.multi_select,
            })
            .collect(),
        answers: q
            .answers
            .iter()
            .map(|a| detail::Answer { question: a.question.clone(), answer: a.answer.clone() })
            .collect(),
    }
}

fn answerer(by: ClientId) -> Answerer {
    if by == ClientId::nil() {
        return Answerer { client: None, name: "orchestration".to_owned() };
    }
    Answerer { client: Some(by), name: String::new() }
}

/// What "allow always" grants, worded as Claude Code's own dialog words it, to follow the
/// button's "Always allow": "edits in /work this session", "access to /work in this project",
/// "npm test commands in this project". A mode leads, in the folders granted with it; rules
/// follow; an update of a kind this worker does not know is said in words. Each part ends with
/// how long it holds, from where Claude Code keeps it.
fn grants(prompt: &PermissionPrompt) -> Option<String> {
    let (mut mode, mut dirs, mut rules, mut others) = (None, Vec::new(), Vec::new(), Vec::new());
    let (mut held, mut rules_held) = (None, None);
    for suggestion in &prompt.suggestions {
        let kept = suggestion.destination.as_deref();
        match &suggestion.grant {
            Grant::Mode { mode: m } => {
                let said = mode_words(m);
                if mode.is_none() && !said.is_empty() {
                    mode = Some(said);
                    held = held.or(kept);
                }
            }
            Grant::Directories { directories } => {
                let named: Vec<&str> =
                    directories.iter().map(|d| d.trim()).filter(|d| !d.is_empty()).collect();
                if !named.is_empty() {
                    dirs.extend(named);
                    held = held.or(kept);
                }
            }
            Grant::Rules { rules: r, .. } => {
                let said: Vec<String> = r.iter().filter_map(|rule| rule_words(rule)).collect();
                if !said.is_empty() {
                    rules.extend(said);
                    rules_held = rules_held.or(kept);
                }
            }
            Grant::Other { kind } => others.push(words(kind)),
        }
    }
    let subject = match (mode, dirs.is_empty()) {
        (Some(mode), true) => Some(mode),
        (Some(mode), false) => Some(format!("{mode} in {}", dirs.join(", "))),
        (None, false) => Some(format!("access to {}", dirs.join(", "))),
        (None, true) => None,
    };
    let parts: Vec<String> = subject
        .map(|subject| format!("{subject}{}", for_how_long(held)))
        .into_iter()
        .chain(
            (!rules.is_empty())
                .then(|| format!("{}{}", rules.join(", "), for_how_long(rules_held))),
        )
        .chain(others.into_iter().filter(|o| !o.is_empty()))
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// The mode a plan's approval may switch to, as [`mode_words`] says it ("edits" for
/// `acceptEdits`): the first `setMode` the request suggests, other than plan mode itself.
fn plan_mode(prompt: &PermissionPrompt) -> Option<String> {
    prompt.suggestions.iter().find_map(|s| match &s.grant {
        Grant::Mode { mode } if mode.trim() != "plan" => {
            Some(mode_words(mode)).filter(|said| !said.is_empty())
        }
        _ => None,
    })
}

/// A permission mode as what it lets through: `acceptEdits` is "edits", as Claude Code's
/// "allow all edits during this session" says it; any other mode is named ("plan mode").
fn mode_words(mode: &str) -> String {
    match mode.trim() {
        "acceptEdits" => "edits".to_owned(),
        mode => {
            let said = words(mode);
            if said.is_empty() { said } else { format!("{said} mode") }
        }
    }
}

/// A permission rule as Claude Code's dialog says it: a command prefix as "npm test commands",
/// a whole command as itself, any other rule as written. `None` for an empty rule.
fn rule_words(rule: &str) -> Option<String> {
    let rule = rule.trim();
    let command = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')).map(str::trim);
    let said = match command {
        Some(command) => match command.strip_suffix(":*").or_else(|| command.strip_suffix(" *")) {
            Some(prefix) => format!("{} commands", prefix.trim()),
            None => command.to_owned(),
        },
        None => rule.to_owned(),
    };
    (!said.is_empty()).then_some(said)
}

/// How long a grant kept at `destination` holds, as words that end its line: the session's,
/// this project's (its local or shared settings), every project's.
fn for_how_long(destination: Option<&str>) -> &'static str {
    match destination {
        Some("session") => " this session",
        Some("localSettings" | "projectSettings") => " in this project",
        Some("userSettings") => " in every project",
        _ => "",
    }
}

/// An open name (a mode, an update's kind) in lower-case words: `acceptEdits` and
/// `AcceptEdits` read "accept edits", `dont-ask` "dont ask". The same split as the client's
/// sentence case, so a name the worker does not know still reads as words.
fn words(name: &str) -> String {
    let mut out = String::with_capacity(name.len().saturating_add(4));
    let mut prev: Option<char> = None;
    for c in name.trim().chars() {
        match c {
            '-' | '_' | ' ' => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            c if c.is_uppercase() => {
                if prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit())
                    && !out.ends_with(' ')
                {
                    out.push(' ');
                }
                out.extend(c.to_lowercase());
            }
            c => out.push(c),
        }
        prev = Some(c);
    }
    out.truncate(out.trim_end().len());
    out
}

/// The allow of a decline's request: the model may try the call again.
pub const TRY_AGAIN: &str = "Let it try again";

/// The deny of a decline's request: the decline stands.
pub const KEEP_DECLINED: &str = "Keep it declined";

/// What a decline's `reason` says in words: the rule a classifier verdict names, out of its
/// brackets (`[Data Exfiltration]`), or the reason as Claude Code gave it.
fn rule(reason: &str) -> &str {
    let reason = reason.trim();
    reason.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(reason).trim()
}

/// The notice a decline is said by: what was turned down, then why.
fn decline_notice(declined: &conv::Declined) -> Notice {
    let words = permission_words(&declined.tool, declined.line.as_deref());
    let mut text = if words.is_empty() {
        "Auto mode declined a call".to_owned()
    } else {
        format!("Auto mode declined: {words}")
    };
    let rule = rule(&declined.reason);
    if !rule.is_empty() {
        text.push('\n');
        text.push_str(rule);
    }
    Notice::new(Notice::DECLINED, Clipped::whole(&text))
}

/// The title of a yes or no on a call: the file an edit or a write is to, by its name ("Allow
/// edit of `lib.rs`?"), else the tool ("Allow Bash?").
fn approval_title(prompt: &PermissionPrompt) -> String {
    let named = |path: &str| path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(path).to_owned();
    match &prompt.detail {
        conv::ToolDetail::Edit(e) if !e.path.is_empty() => {
            format!("Allow edit of {}?", named(&e.path))
        }
        conv::ToolDetail::Write(w) if !w.path.is_empty() => {
            format!("Allow write of {}?", named(&w.path))
        }
        _ => format!("Allow {}?", prompt.tool),
    }
}

/// A held permission prompt as a request, with the answers Claude Code takes.
fn request(prompt: &PermissionPrompt) -> Request {
    let thread = conv::ThreadId::Main;
    let choice = |id: &str, label: &str, effect, scope, stops| Choice {
        id: id.to_owned(),
        label: label.to_owned(),
        effect,
        scope,
        stops,
    };
    let deny = choice("deny", "Deny", Effect::Deny, None, false);
    let deny_stop = choice("deny-stop", "Deny and stop", Effect::Deny, None, true);
    let (kind, title, options, questions, proposed) = match (&prompt.declined, &prompt.detail) {
        (Some(reason), detail) => {
            let options = vec![
                choice("deny", KEEP_DECLINED, Effect::Deny, None, false),
                choice("allow", TRY_AGAIN, Effect::Allow, None, false),
            ];
            let proposed = match detail {
                conv::ToolDetail::Edit(e) => Some(e.patch.clone()),
                conv::ToolDetail::Write(w) => Some(w.patch.clone()),
                _ => None,
            };
            let title = match rule(reason) {
                "" => format!("Auto mode declined {}", prompt.tool),
                rule => format!("Auto mode declined {}: {rule}", prompt.tool),
            };
            (Request::RETRY, title, options, Vec::new(), proposed)
        }
        (None, detail) => match detail {
            conv::ToolDetail::Question(q) => {
                let title =
                    q.questions.first().map_or_else(|| "A question".to_owned(), |q| q.text.clone());
                (Request::QUESTION, title, vec![deny], question(q).questions, None)
            }
            conv::ToolDetail::Plan { .. } => {
                let allow = choice("allow", "Approve the plan", Effect::Allow, None, false);
                // Claude Code's own plan dialog offers to accept edits from then on: the mode
                // its request suggests, carried back as "Always allow" carries its suggestions.
                let and_then = plan_mode(prompt).map(|mode| {
                    let label = match mode.as_str() {
                        "edits" => "Approve, and accept edits".to_owned(),
                        mode => format!("Approve, and switch to {mode}"),
                    };
                    choice("always", &label, Effect::Allow, grants(prompt), false)
                });
                let options = [Some(allow), and_then, Some(deny)].into_iter().flatten().collect();
                (Request::PLAN, "Approve the plan?".to_owned(), options, Vec::new(), None)
            }
            detail => {
                let mut options = vec![choice("allow", "Allow", Effect::Allow, None, false)];
                if !prompt.suggestions.is_empty() {
                    options.push(choice(
                        "always",
                        "Always allow",
                        Effect::Allow,
                        grants(prompt),
                        false,
                    ));
                }
                options.extend([deny, deny_stop]);
                let proposed = match detail {
                    conv::ToolDetail::Edit(e) => Some(e.patch.clone()),
                    conv::ToolDetail::Write(w) => Some(w.patch.clone()),
                    _ => None,
                };
                (Request::APPROVAL, approval_title(prompt), options, Vec::new(), proposed)
            }
        },
    };
    let text = match &prompt.detail {
        conv::ToolDetail::Bash(b) => Some(clipped(&thread, &b.command)),
        conv::ToolDetail::Plan { plan } => Some(clipped(&thread, plan)),
        _ => None,
    };
    Request {
        id: AskId(prompt.ask.to_string()),
        item: prompt.call.clone().map(ItemId),
        kind: kind.to_owned(),
        title,
        text,
        options,
        questions,
        proposed,
        schema_json: None,
        url: None,
        state: RequestState::Open,
        opened_ms: prompt.asked_ms,
        until_ms: Some(prompt.until_ms),
    }
}

#[cfg(test)]
mod tests;
