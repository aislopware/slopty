//! A Claude Code session's record mapped onto the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Its conversation is read from the session's own transcript or heard as the driven Claude Code
//! streams it ([`super::driven`]), both through the transcript decoder ([`crate::conversation`]).
//!
//! Sans-IO. This turns what the decoder makes of the records into [`Out`]s, the threads to make
//! and the actions to apply, and keeps only what the mapping needs (which turn each entry is in,
//! which subagent hangs off which call). [`replay`] rebuilds a thread from a session's records
//! alone: what opening an old conversation, or a worker that lost its cache, shows.
//!
//! - **Threads.** A Claude Code session is one thread, its id derived from the session's own
//!   ([`thread_of`]), so the same session is the same thread across a worker restart and a
//!   `--resume`. Each subagent is a thread of its own, derived from the session's and its agent id,
//!   linked to the `Agent` call that started it.
//! - **Turns.** A prompt starts the next turn of its thread; every entry after it is in that turn.
//!   The engine's end of a turn ([`History::ended`]) or the transcript's turn record ends it, and a
//!   prompt that comes while a turn is open ends that one. A branch (a rewind, an edited prompt)
//!   truncates the thread back to before the dropped prompt.
//! - **Items.** An entry keeps its id. A tool call's kind comes from the decoder's typed detail,
//!   not from Claude Code's tool names, and its title is worded here.
//! - **Live blocks.** A block the model is writing ([`History::live`]) is an item of its own,
//!   appended to as it grows, until its entry settles it ([`History::settle`]): a tool call's block
//!   takes the call's own id, and a text's or a thinking's entry takes the block's item, so the
//!   entry replaces the block where it stands.
//! - **Requests.** A permission Claude Code asks for is a request with the answers it takes: allow,
//!   allow always (with what the suggestions grant), deny, and deny and stop. [`verdict`] maps a
//!   chosen answer back.

use std::collections::{BTreeMap, HashMap, HashSet};

use slopty_core::WallMs;
use slopty_proto::thread::{
    self, Action, AgentId, AskId, BackgroundTask, Cap, Changed, Choice, Clipped, Compaction,
    ContentRef, Drive, Effect, IntentId, Item, ItemBody, ItemId, Link, Liveness, Meters, Model,
    Notice, PartKey, Pending, Phase, Plan, Request, RequestState, Retry, Status, Step, ThreadId,
    ThreadMeta, ThreadState, ToolCall, ToolState, Turn, TurnId, TurnState, Usage, UserMessage,
    Wait, detail, kind,
};

use crate::conversation::{
    self as conv, Body, Change, Grant, Live, LiveId, LiveKind, NoteKind, PermissionEvent,
    PermissionPrompt, ResultStatus, Settled, TextRef, Verdict,
};

/// What the threads of a session need done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Host this thread from nothing: a new one, or one held from before (a worker restart,
    /// a transcript read again), whose log starts over.
    Begin(Box<ThreadMeta>),
    /// Apply actions to a thread.
    Actions(ThreadId, Vec<Action>),
}

/// How the engine ended a turn ([`History::ended`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// It answered.
    Complete,
    /// It was stopped.
    Interrupted,
    /// It failed, saying this, unless the transcript said why; held until a moment when it may
    /// go again (a usage limit's reset).
    Failed(Option<String>),
}

/// Claude Code's fast mode, offered on each of its models while its `/fast` command is listed:
/// the one option a Claude Code model has ([`Model::options`]).
pub const FAST: &str = "fast";

pub use cards::{PLAN_CLEAR, clear_mode};

/// The words a thread waits with while it is compacted.
pub const COMPACTING_TEXT: &str = "Compacting the conversation";

/// Claude Code's own review ([`Cap::REVIEW`]): its built-in skill, which takes a ref range
/// (`a...b`) and reviews what it changes. A thread can review only while Claude Code lists it.
pub const REVIEW_COMMAND: &str = "code-review";

/// The models a Claude Code takes by the aliases it resolves to its current ones, until it lists
/// its own ([`History::models`]).
pub const MODELS: [(&str, &str); 4] =
    [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku"), ("default", "Default")];

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

/// What a permission for `tool` with `input` waits on, as a row leads with it ("Run cargo
/// test", "Edit src/main.rs"), from the call's own line.
#[must_use]
pub fn asks_for(tool: &str, input: &serde_json::Value) -> String {
    if tool == "AskUserQuestion" {
        let asked = input
            .get("questions")
            .and_then(serde_json::Value::as_array)
            .and_then(|q| q.first())
            .and_then(|q| q.get("question"))
            .and_then(serde_json::Value::as_str);
        return asked.map_or_else(|| "Has a question".to_owned(), str::to_owned);
    }
    let line =
        ["command", "file_path", "notebook_path", "path", "url", "query", "pattern", "description"]
            .iter()
            .find_map(|key| input.get(*key).and_then(serde_json::Value::as_str))
            .map(|line| line.lines().next().unwrap_or_default().to_owned());
    permission_words(tool, line.as_deref())
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

/// What a subagent that stopped with background work of its own still running waits on.
pub const WAITING_TEXT: &str = "Waiting on its own background work";

/// The thread of Claude Code session `native`.
#[must_use]
pub fn thread_of(native: &str) -> ThreadId {
    ThreadId::derived(&["claude-code session", native])
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

/// The choice that answers an agent's questions with none of their answers, to talk them over
/// first (the TUI's "Chat about this").
pub const CLARIFY: &str = "clarify";

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
    /// Turns the engine ended, as it ended them: the transcript's record of one changes it no
    /// more.
    by_engine: HashMap<TurnId, TurnState>,
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
    /// The entries that settled a live text or thinking block, and that block's item, which
    /// they are shown as: where the block stood, not after what began since.
    shown_as: HashMap<String, ItemId>,
    /// A subagent's status as last told ([`History::settle_subagents`]).
    told: Option<Status>,
    /// What each call's header and line said in the TUI's words, as last noted: a call is noted
    /// again only when they change.
    worded: HashMap<String, (Option<String>, Option<String>)>,
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
            by_engine: HashMap::new(),
            turns: HashMap::new(),
            background: HashMap::new(),
            calls: HashMap::new(),
            tasks: Vec::new(),
            shown_as: HashMap::new(),
            told: None,
            worded: HashMap::new(),
        }
    }

    /// Put `task` in the list in place of the one its call started before, or drop that one
    /// when the call starts none now; whether the list changed.
    fn set_task(&mut self, call: &str, task: Option<BackgroundTask>) -> bool {
        let at = self.tasks.iter().position(|t| t.item.as_ref().is_some_and(|i| i.0 == call));
        // What the engine said of the task live is kept under what the transcript says of it.
        let task = task.map(|mut task| {
            if let Some(had) = at.and_then(|at| self.tasks.get(at)) {
                merge_live(had, &mut task);
            }
            task
        });
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

    /// A task's notice tells its task how it stands; whether the list changed. A monitor's
    /// event leaves it running.
    fn note_task(&mut self, note: &conv::TaskNotice, at: WallMs) -> bool {
        let Some(task) = self.tasks.iter_mut().find(|t| t.id == note.task_id) else {
            return false;
        };
        let before = task.clone();
        let state = match note.status.as_str() {
            "failed" => Some(BackgroundTask::FAILED),
            "killed" => Some(BackgroundTask::KILLED),
            "waiting" => Some(BackgroundTask::WAITING),
            "completed" | "expired" => Some(BackgroundTask::COMPLETED),
            _ => None,
        };
        if let Some(state) = state {
            state.clone_into(&mut task.state);
            task.outcome = Some(outcome_of(&note.summary)).filter(|o| !o.is_empty());
            if state != BackgroundTask::WAITING && !at.is_zero() {
                task.ended_ms = task.ended_ms.or(Some(at));
            }
        }
        if let Some(usage) = task_usage(note.usage) {
            task.usage = Some(usage);
        }
        *task != before
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

/// The `Agent` call that started a subagent, as last read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spawn {
    /// The decoder's thread the call is in.
    thread: conv::ThreadId,
    /// The call's id.
    call: String,
    /// What the model said the subagent is for: its thread's title.
    title: String,
    /// When the call started it.
    at_ms: WallMs,
    /// How it ended, once it did, and when.
    end: Option<(SubagentEnd, WallMs)>,
    /// It stopped with background work of its own still running, and may go on by itself.
    waiting: bool,
}

impl Spawn {
    /// The call `entry` (`call`, starting `agent`) in the decoder's `thread`.
    fn of(
        thread: &conv::ThreadId,
        entry: &conv::Entry,
        call: &conv::ToolCall,
        agent: &conv::AgentDetail,
    ) -> Self {
        // One run in the background ends when Claude Code says it did, after its run; one in
        // the foreground with its call's result.
        let ran = agent
            .duration_ms
            .map(|ms| WallMs::from_millis(entry.at_ms.as_millis().saturating_add(ms)));
        let at_ms = ran.or_else(|| call.result.as_ref().map(|r| r.at_ms)).unwrap_or(entry.at_ms);
        let end = match agent.status {
            conv::AgentRun::Running | conv::AgentRun::Waiting => None,
            conv::AgentRun::Completed => Some(SubagentEnd::Completed),
            conv::AgentRun::Killed => Some(SubagentEnd::Stopped),
            conv::AgentRun::Failed => {
                let said = agent.report.as_ref().or_else(|| call.result.as_ref()?.text.as_ref());
                // Its first line, as a turn's error is said.
                let error = said
                    .map(|text| title_of(&text.text))
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| "The subagent failed".to_owned());
                Some(SubagentEnd::Failed(error))
            }
        };
        Self {
            thread: thread.clone(),
            call: entry.id.clone(),
            title: agent.description.clone().unwrap_or_default(),
            at_ms: entry.at_ms,
            end: end.map(|end| (end, at_ms)),
            waiting: agent.status == conv::AgentRun::Waiting,
        }
    }
}

/// How a subagent ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SubagentEnd {
    /// It reported back.
    Completed,
    /// It was stopped.
    Stopped,
    /// It failed, saying this.
    Failed(String),
}

impl SubagentEnd {
    /// Its turn's end and its phase.
    fn ended(&self) -> (TurnState, Phase) {
        match self {
            Self::Completed => (TurnState::Complete, Phase::Done),
            Self::Stopped => (TurnState::Interrupted, Phase::Stopped),
            Self::Failed(error) => {
                (TurnState::Failed { error: error.clone(), until_ms: None }, Phase::Failed)
            }
        }
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

/// What an agent at rest waits on: the tasks left running and the scheduled prompts, and the
/// engine's words for the first task.
#[derive(Debug)]
struct Waiting {
    tasks: u32,
    crons: u32,
    named: Option<String>,
}

/// One Claude Code session as threads.
#[derive(Debug)]
pub struct History {
    meta: ThreadMeta,
    threads: HashMap<conv::ThreadId, Mapped>,
    /// Each subagent's call, by the subagent's id.
    spawned: HashMap<String, Spawn>,
    live: HashMap<LiveId, Provisional>,
    /// Live blocks held until their prompt opens their turn, oldest first.
    deferred: Vec<Deferred>,
    meters: Meters,
    /// The status last told, and the requests open: a thread begun anew from the transcript
    /// gets them again, since the transcript has neither.
    status: Option<Status>,
    open: Vec<Request>,
    /// What the agent waits on at rest (background tasks, scheduled prompts), while it waits:
    /// the wait is worded again as the transcript names those tasks.
    waiting_on: Option<Waiting>,
    out: Vec<Out>,
    /// The slash commands as last told.
    commands: Vec<thread::Command>,
    /// The person's and the project's own commands on disk (`crate::commands::custom`).
    custom: Vec<conv::SlashCommand>,
    /// Claude Code's own list of its commands and skills, as it last said it.
    listed: Option<Vec<crate::commands::Listed>>,
    /// The main thread's last turn ended failed, so its agent's done is a failure.
    failed: bool,
    /// The wall clock as a live block was last heard: when a live block shows.
    wall: WallMs,
    /// When the Claude Code that runs the session now started ([`Self::started`]).
    started: Option<WallMs>,
    /// The intent that sent each of the person's messages Slopty gave the engine, by the
    /// message's id ([`Self::sent_as`]).
    intents: HashMap<String, IntentId>,
    /// A stop under way, worded as far as it went ([`Wait::STOPPING`]).
    stopping: Option<String>,
    /// The turn under way shows no progress, worded ([`Wait::STALLED`]).
    stalled: Option<String>,
    /// Its context is being compacted ([`Wait::COMPACTING`]).
    compacting: bool,
    /// Held at its plan's usage limit until then ([`Wait::LIMIT`]).
    limited: Option<WallMs>,
    /// When the latest entry was made.
    last_at: WallMs,
    /// The main thread's request being tried again ([`Self::retrying`]): its one row, as it
    /// stands.
    retry: Option<Item>,
    /// The MCP Apps the session's servers offer, by server (as its tools are named) and tool
    /// ([`Self::set_apps`]).
    apps: HashMap<(String, String), slopty_proto::visual::McpApp>,
    /// Notices said so far, so two in one millisecond are two rows.
    notices: u64,
    /// The resources each task's result linked, by the task, for its row.
    task_links: HashMap<String, Vec<slopty_proto::visual::ResourceLink>>,
}

impl History {
    /// Session `native` of Claude Code `version` in `cwd`, as Slopty drives it, with
    /// capabilities `caps`. The first outs it gives, from whichever call, begin its thread.
    #[must_use]
    pub fn new(native: &str, version: &str, cwd: &str, caps: &[&str], now: WallMs) -> Self {
        let offers = super::offers::offers();
        let meta = ThreadMeta {
            modes: offers.modes,
            efforts: offers.efforts,
            id: thread_of(native),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: version.to_owned(),
            native: native.to_owned(),
            cwd: cwd.to_owned(),
            title: String::new(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::DRIVEN),
            caps: Vec::new(),
            models: offers.models,
            facts: BTreeMap::new(),
            created_ms: now,
        };
        Self::of(meta, caps)
    }

    /// The thread `meta` as it stands, read again from nothing: what it is and who holds it are
    /// kept, what it holds is told again by the records that follow.
    #[must_use]
    pub fn of(meta: ThreadMeta, caps: &[&str]) -> Self {
        let mut threads = HashMap::new();
        threads.insert(conv::ThreadId::Main, Mapped::new(meta.id));
        let mut history = Self {
            out: Vec::new(),
            meta,
            threads,
            spawned: HashMap::new(),
            live: HashMap::new(),
            deferred: Vec::new(),
            meters: Meters::default(),
            status: None,
            open: Vec::new(),
            waiting_on: None,
            commands: Vec::new(),
            custom: Vec::new(),
            listed: None,
            failed: false,
            wall: WallMs::ZERO,
            started: None,
            intents: HashMap::new(),
            stopping: None,
            stalled: None,
            compacting: false,
            limited: None,
            last_at: WallMs::ZERO,
            retry: None,
            apps: HashMap::new(),
            notices: 0,
            task_links: HashMap::new(),
        };
        history.meta.caps = history.caps_with(caps);
        history.out.push(Out::Begin(Box::new(history.meta.clone())));
        history
    }

    /// `base`, and [`Cap::REVIEW`] while Claude Code lists its review and a message can reach
    /// it.
    fn caps_with(&self, base: &[&str]) -> Vec<Cap> {
        let mut caps: Vec<Cap> = base.iter().copied().map(Cap::named).collect();
        let sends = caps.contains(&Cap::named(Cap::QUEUE));
        if sends && self.commands.iter().any(|c| c.name == REVIEW_COMMAND) {
            caps.push(Cap::named(Cap::REVIEW));
        }
        caps.sort();
        caps.dedup();
        caps
    }

    /// The capabilities are `caps` now (and review where it is listed).
    pub fn set_caps(&mut self, caps: &[&str]) -> Vec<Out> {
        let caps = self.caps_with(caps);
        if caps != self.meta.caps {
            self.meta.caps = caps;
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// Keep `value` as the thread's fact `name`, or forget it for `None`.
    pub fn fact(&mut self, name: &str, value: Option<&str>) -> Vec<Out> {
        let had = self.meta.facts.get(name).map(String::as_str);
        if had != value {
            match value {
                Some(value) => drop(self.meta.facts.insert(name.to_owned(), value.to_owned())),
                None => drop(self.meta.facts.remove(name)),
            }
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// `status` as the thread shows it: what it waits on beside its phase (a stop under way,
    /// a compaction, the plan's limit, no progress).
    fn shown(&self, status: Option<Status>) -> Option<Status> {
        let mut status = status?;
        if status.liveness != Liveness::Live {
            return Some(status);
        }
        let at_work = matches!(status.phase, Phase::Working | Phase::NeedsYou | Phase::Waiting);
        if let Some(text) = self.stopping.as_ref().filter(|_| at_work) {
            status.phase = Phase::Working;
            status.wait = Some(Wait { kind: Wait::STOPPING.to_owned(), text: text.clone() });
        } else if self.compacting {
            status.phase = Phase::Working;
            let text = COMPACTING_TEXT.to_owned();
            status.wait = Some(Wait { kind: Wait::COMPACTING.to_owned(), text });
        } else if self.limited.is_some() && status.phase != Phase::NeedsYou {
            status.phase = Phase::Waiting;
            status.wait = Some(Wait { kind: Wait::LIMIT.to_owned(), text: LIMIT_TEXT.to_owned() });
        } else if let Some(text) = self.stalled.as_ref().filter(|_| status.phase == Phase::Working)
        {
            status.wait = Some(Wait { kind: Wait::STALLED.to_owned(), text: text.clone() });
        }
        Some(status)
    }

    /// Tell the thread its status again as [`Self::shown`] shows it, after an overlay moved.
    fn reshow(&mut self) {
        if let Some(shown) = self.shown(self.status.clone()) {
            self.push(self.meta.id, Action::Status(shown));
        }
    }

    /// The engine is at `phase` since `since_ms`, alive or not; what it waits on, if anything,
    /// is `wait` (a permission, a question), else what the thread's overlays say.
    pub fn status(
        &mut self,
        phase: Phase,
        wait: Option<Wait>,
        liveness: Liveness,
        since_ms: WallMs,
    ) -> Vec<Out> {
        let phase = if phase == Phase::Done && self.failed { Phase::Failed } else { phase };
        let (phase, wait) = match (phase, wait, self.waiting_on.as_ref()) {
            (Phase::Done | Phase::Idle, None, Some(waiting)) if liveness == Liveness::Live => {
                (Phase::Waiting, Some(self.wait_on(waiting)))
            }
            (phase, wait, _) => (phase, wait),
        };
        let status = Status { phase, wait, liveness, since_ms };
        let same = self.status.as_ref().is_some_and(|had| {
            had.phase == status.phase && had.wait == status.wait && had.liveness == status.liveness
        });
        if !same {
            self.status = Some(status);
            self.reshow();
        }
        self.drain()
    }

    /// The status last told.
    #[must_use]
    pub const fn phase(&self) -> Option<&Status> {
        self.status.as_ref()
    }

    /// What the engine left running at rest: `tasks` background tasks and `crons` scheduled
    /// prompts, the first named `named`. The thread waits on them once its turn ends.
    pub fn background(&mut self, tasks: u32, crons: u32, named: Option<String>) -> Vec<Out> {
        self.waiting_on = (tasks > 0 || crons > 0).then_some(Waiting { tasks, crons, named });
        self.rewait();
        self.drain()
    }

    /// A task the engine says it started, in the session's list: in place of the one its call
    /// started, else of its id, else after the others.
    pub fn task_started(&mut self, task: BackgroundTask) -> Vec<Out> {
        let id = self.meta.id;
        let Some(mapped) = self.threads.get_mut(&conv::ThreadId::Main) else {
            return self.drain();
        };
        let at = mapped
            .tasks
            .iter()
            .position(|t| t.id == task.id || (task.item.is_some() && t.item == task.item));
        let changed = if let Some(had) = at.and_then(|at| mapped.tasks.get_mut(at)) {
            let before = had.clone();
            let mut merged = task;
            // What the transcript said already stands; the engine adds what it alone knows.
            merged.state.clone_from(&had.state);
            merged.output.clone_from(&had.output);
            merged.ended_ms = had.ended_ms;
            merged.started_ms = had.started_ms.min(merged.started_ms);
            merged.title = if had.title.is_empty() { merged.title } else { had.title.clone() };
            merge_live(&before, &mut merged);
            *had = merged;
            *had != before
        } else {
            mapped.tasks.push(task);
            true
        };
        if changed {
            let tasks = mapped.tasks.clone();
            self.push(id, Action::TasksSet(tasks));
        }
        self.drain()
    }

    /// Change task `task` as `change` says, when the session lists it; tell the list again
    /// when that changed it.
    pub fn task_changed(
        &mut self,
        task: &str,
        change: impl FnOnce(&mut BackgroundTask),
    ) -> Vec<Out> {
        let id = self.meta.id;
        let Some(mapped) = self.threads.get_mut(&conv::ThreadId::Main) else {
            return self.drain();
        };
        if let Some(had) = mapped.tasks.iter_mut().find(|t| t.id == task) {
            let before = had.clone();
            change(had);
            if *had != before {
                let tasks = mapped.tasks.clone();
                self.push(id, Action::TasksSet(tasks));
            }
        }
        self.drain()
    }

    /// The tasks the engine says run now, by id: one listed running that is not among them
    /// ended at `now`, however its notice says it did once that comes.
    pub fn tasks_live(&mut self, live: &[String], now: WallMs) -> Vec<Out> {
        let id = self.meta.id;
        let Some(mapped) = self.threads.get_mut(&conv::ThreadId::Main) else {
            return self.drain();
        };
        let mut changed = false;
        for task in &mut mapped.tasks {
            // A subagent waiting on its own work is not in the engine's list, and still waits.
            let gone = task.is_live()
                && task.state != BackgroundTask::WAITING
                && task.backgrounded
                && !live.contains(&task.id);
            if gone {
                BackgroundTask::COMPLETED.clone_into(&mut task.state);
                task.ended_ms = task.ended_ms.or(Some(now));
                changed = true;
            }
        }
        if changed {
            let tasks = mapped.tasks.clone();
            self.push(id, Action::TasksSet(tasks));
        }
        self.drain()
    }

    /// The resources task `task`'s result linked, for the row it ends with.
    pub fn task_links(&mut self, task: &str, links: Vec<slopty_proto::visual::ResourceLink>) {
        if !links.is_empty() {
            self.task_links.insert(task.to_owned(), links);
        }
    }

    /// Call `call` says how it goes, `note`: on the thread it is in.
    pub fn call_noted(&mut self, call: &str, note: thread::CallNote) -> Vec<Out> {
        let thread = self
            .threads
            .values()
            .find(|m| m.calls.contains_key(call) || m.items.contains_key(call))
            .map(|m| m.id);
        if let Some(thread) = thread {
            self.push(thread, Action::CallNoted { item: ItemId(call.to_owned()), note });
        }
        self.drain()
    }

    /// Whether call `call` runs still, as far as the record says.
    #[must_use]
    pub fn call_open(&self, call: &str) -> bool {
        self.threads.values().any(|m| m.calls.contains_key(call))
    }

    /// A request of the agent's own beside its permissions (a dialog, a form): open now.
    pub fn asked(&mut self, request: Request) -> Vec<Out> {
        self.open.retain(|r| r.id != request.id);
        self.open.push(request.clone());
        self.push(self.meta.id, Action::RequestOpened(Box::new(request)));
        self.drain()
    }

    /// Request `id` stands as `state` now.
    pub fn resolved(&mut self, id: &AskId, state: RequestState) -> Vec<Out> {
        if self.open.iter().any(|r| r.id == *id) {
            self.open.retain(|r| r.id != *id);
            self.push(self.meta.id, Action::RequestResolved { id: id.clone(), state });
        }
        self.drain()
    }

    /// The MCP Apps the session's servers offer now, by server (as the session names its
    /// tools, `mcp__<server>__<tool>`) and tool: a call of one shows its page from then on.
    pub fn set_apps(&mut self, apps: HashMap<(String, String), slopty_proto::visual::McpApp>) {
        self.apps = apps;
    }

    /// The record that began turn `turn` of the session's own thread: its prompt's record id.
    #[must_use]
    pub fn prompt_of(&self, turn: TurnId) -> Option<String> {
        let mapped = self.threads.get(&conv::ThreadId::Main)?;
        let (id, _) = mapped.prompts.iter().find(|(_, t)| **t == turn)?;
        Some(id.split_once(':').map_or(id.as_str(), |(record, _)| record).to_owned())
    }

    /// `shown` with its paths made the worker's own: a relative path is under the session's
    /// folder.
    fn rooted(&self, shown: &detail::ToolDetail) -> detail::ToolDetail {
        let cwd = std::path::Path::new(&self.meta.cwd);
        let rooted = |path: &str| {
            if std::path::Path::new(path).is_absolute() || path.starts_with("~/") {
                path.to_owned()
            } else {
                cwd.join(path).to_string_lossy().into_owned()
            }
        };
        match shown.clone() {
            detail::ToolDetail::Visual(mut visual) => {
                for path in visual.assets.values_mut() {
                    *path = rooted(path);
                }
                detail::ToolDetail::Visual(visual)
            }
            detail::ToolDetail::Page(mut page) => {
                if let slopty_proto::visual::PageSource::File(file) = &mut page.source {
                    *file = rooted(file);
                }
                detail::ToolDetail::Page(page)
            }
            other => other,
        }
    }

    /// Any action on the session's own thread, told as it is.
    pub fn tell(&mut self, action: Action) -> Vec<Out> {
        self.push(self.meta.id, action);
        self.drain()
    }

    /// The session's list of tasks as it stands.
    #[must_use]
    pub fn tasks_now(&self) -> &[BackgroundTask] {
        self.threads.get(&conv::ThreadId::Main).map_or(&[], |m| m.tasks.as_slice())
    }

    /// The turn entries go to now in the session's own thread.
    #[must_use]
    pub fn turn_now(&self) -> TurnId {
        self.threads.get(&conv::ThreadId::Main).map_or(TurnId::BEFORE, |m| m.turn)
    }

    /// A stop under way, worded as far as it went, or `None` once it ended: the thread waits on
    /// it ([`Wait::STOPPING`]) while it works.
    pub fn stopping(&mut self, text: Option<String>) -> Vec<Out> {
        if self.stopping != text {
            self.stopping = text;
            self.reshow();
        }
        self.drain()
    }

    /// The turn under way shows no progress, worded, or `None` once it moved again
    /// ([`Wait::STALLED`]). Nothing is done about it but saying so.
    pub fn stalled(&mut self, text: Option<String>) -> Vec<Out> {
        if self.stalled != text {
            self.stalled = text;
            self.reshow();
        }
        self.drain()
    }

    /// Its context is being compacted, or was ([`Wait::COMPACTING`]).
    pub fn compacting(&mut self, on: bool) -> Vec<Out> {
        if self.compacting != on {
            self.compacting = on;
            self.reshow();
        }
        self.drain()
    }

    /// Held at the plan's usage limit until `until`, or no more for `None` ([`Wait::LIMIT`]).
    pub fn limited(&mut self, until: Option<WallMs>) -> Vec<Out> {
        if self.limited != until {
            self.limited = until;
            self.reshow();
        }
        self.drain()
    }

    /// The person's message `id` (the record Claude Code writes of it) was sent as `intent`:
    /// its item says so.
    pub fn sent_as(&mut self, id: &str, intent: IntentId) {
        self.intents.insert(id.to_owned(), intent);
    }

    /// The engine's main loop began a turn at `now`: the thread works, unless it already waits
    /// on the person.
    pub fn working(&mut self, now: WallMs) -> Vec<Out> {
        let status = self.status.get_or_insert(Status {
            phase: Phase::Idle,
            wait: None,
            liveness: Liveness::Live,
            since_ms: now,
        });
        if matches!(status.phase, Phase::Working | Phase::NeedsYou) {
            return self.drain();
        }
        *status =
            Status { phase: Phase::Working, wait: None, liveness: Liveness::Live, since_ms: now };
        self.failed = false;
        self.reshow();
        self.drain()
    }

    /// The engine said the main loop's turn under way ended, as `end` says, at `now`: the turn
    /// open ends so, and a transcript record of it that comes after changes that no more. The
    /// thread rests: stopped after an interruption, failed after an error, else done.
    pub fn ended(&mut self, end: &End, now: WallMs) -> Vec<Out> {
        let phase = match end {
            End::Interrupted => Phase::Stopped,
            End::Failed(_) => Phase::Failed,
            End::Complete => Phase::Done,
        };
        let limited = self.limited;
        let id = self.meta.id;
        let mut actions = Vec::new();
        // A request still being tried again: a failed turn ran out of attempts, and the row is
        // its error now; any other end leaves nothing to try.
        if let Some(mut item) = self.retry.take() {
            if matches!(end, End::Failed(_)) {
                if let ItemBody::Notice(notice) = &mut item.body {
                    notice.retry = None;
                }
                item.at_ms = now;
                actions.push(Action::ItemCompleted(item));
            } else {
                actions.push(Action::ItemRemoved { item: item.id });
            }
        }
        if let Some(mapped) = self.threads.get_mut(&conv::ThreadId::Main) {
            let turn = mapped.turn;
            if turn != TurnId::BEFORE && !mapped.ended.contains(&turn) {
                let state = match end {
                    End::Interrupted => TurnState::Interrupted,
                    End::Failed(said) => {
                        let (error, until_ms) = mapped.stopped.get(&turn).map_or_else(
                            || {
                                let said = said.clone();
                                (
                                    said.unwrap_or_else(|| {
                                        "Claude Code's request failed".to_owned()
                                    }),
                                    limited,
                                )
                            },
                            |(stop, error)| (error.clone(), stop.until_ms.or(limited)),
                        );
                        TurnState::Failed { error, until_ms }
                    }
                    End::Complete => TurnState::Complete,
                };
                mapped.ended.insert(turn);
                mapped.by_engine.insert(turn, state.clone());
                let usage = mapped.turns.get(&turn).map(|t| t.usage.clone()).unwrap_or_default();
                if let Some(t) = mapped.turns.get_mut(&turn) {
                    t.state = state.clone();
                    t.ended_ms = Some(now);
                }
                actions.push(Action::TurnEnded { turn, state, usage, ended_ms: now });
            }
        }
        for action in actions {
            self.push(id, action);
        }
        self.failed = phase == Phase::Failed;
        if let Some(status) = self.status.as_mut()
            && matches!(status.phase, Phase::Working | Phase::NeedsYou | Phase::Waiting)
            && status.liveness == Liveness::Live
        {
            *status = Status { phase, wait: None, liveness: Liveness::Live, since_ms: now };
            if let Some(waiting) = self.waiting_on.as_ref() {
                let wait = self.wait_on(waiting);
                if let Some(status) = self.status.as_mut() {
                    status.phase = Phase::Waiting;
                    status.wait = Some(wait);
                }
            }
            self.reshow();
        }
        self.drain()
    }

    /// The worker ended the turn under way itself, at `now`, when a stop did not reach the
    /// engine's own end ([`Self::ended`]): the turn is interrupted, saying `how`.
    pub fn cut(&mut self, how: &str, now: WallMs) -> Vec<Out> {
        let open = self
            .threads
            .get(&conv::ThreadId::Main)
            .map(|m| m.turn)
            .filter(|turn| *turn != TurnId::BEFORE);
        let ended = self
            .threads
            .get(&conv::ThreadId::Main)
            .is_some_and(|m| open.is_some_and(|turn| m.ended.contains(&turn)));
        let mut outs = Vec::new();
        if !ended {
            outs.extend(self.ended(&End::Interrupted, now));
        }
        if let Some(turn) = open.filter(|_| !ended) {
            let item = Item {
                id: ItemId(format!("stopped-{}", now.as_millis())),
                turn,
                at_ms: now,
                body: ItemBody::Notice(Notice::new(Notice::INTERRUPTED, Clipped::whole(how))),
            };
            self.push(self.meta.id, Action::ItemCompleted(item));
        }
        outs.extend(self.drain());
        outs
    }

    /// What the person's messages on their way and held are now.
    pub fn pending(&mut self, pending: Vec<Pending>) -> Vec<Out> {
        self.push(self.meta.id, Action::PendingSet(pending));
        self.drain()
    }

    /// The thread branched off `fork`'s conversation, which it holds the history of.
    pub fn forked(&mut self, fork: thread::Fork) -> Vec<Out> {
        self.meta.forked_from = Some(fork);
        ThreadMeta::FORK.clone_into(&mut self.meta.origin);
        self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        self.drain()
    }

    /// The Claude Code that runs the session is version `version`.
    pub fn version(&mut self, version: &str) -> Vec<Out> {
        if self.meta.agent_version != version {
            version.clone_into(&mut self.meta.agent_version);
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// Claude Code named the session `title`.
    pub fn named(&mut self, title: &str) -> Vec<Out> {
        let title = title.trim();
        if !title.is_empty() && self.meta.title != title {
            title.clone_into(&mut self.meta.title);
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// Nothing runs at `now`: a turn the records leave open ended there, interrupted when an
    /// interrupt is its last word, failed when an error stopped it, else complete.
    pub fn rest(&mut self, now: WallMs) -> Vec<Out> {
        let end = self.threads.get(&conv::ThreadId::Main).and_then(|m| {
            let turn = m.turn;
            (turn != TurnId::BEFORE && !m.ended.contains(&turn)).then(|| {
                if m.interrupted.contains(&turn) {
                    End::Interrupted
                } else if m.stopped.contains_key(&turn) {
                    End::Failed(None)
                } else {
                    End::Complete
                }
            })
        });
        let at = self
            .threads
            .get(&conv::ThreadId::Main)
            .and_then(|m| m.turns.get(&m.turn))
            .map(|t| t.started_ms)
            .filter(|at| !at.is_zero())
            .map_or(now, |started| self.last_at.max(started));
        let mut outs = Vec::new();
        if let Some(end) = &end {
            outs.extend(self.ended(end, at));
        }
        if self.status.is_none() {
            let phase = match end {
                Some(End::Failed(_)) => Phase::Failed,
                Some(End::Interrupted) => Phase::Stopped,
                Some(End::Complete) => Phase::Done,
                None => Phase::Idle,
            };
            self.status =
                Some(Status { phase, wait: None, liveness: Liveness::Live, since_ms: at });
            self.reshow();
        }
        outs.extend(self.drain());
        outs
    }

    /// Claude Code is gone at `now`: the thread is exited, and goes on by its next message.
    pub fn gone(&mut self, now: WallMs) -> Vec<Out> {
        let phase = self.status.as_ref().map_or(Phase::Idle, |s| match s.phase {
            Phase::Working | Phase::NeedsYou | Phase::Waiting => Phase::Stopped,
            other => other,
        });
        self.status = Some(Status {
            phase,
            wait: None,
            liveness: Liveness::Exited { resumable: true },
            since_ms: now,
        });
        self.reshow();
        self.started = Some(now);
        self.settle_subagents();
        self.drain()
    }

    /// Claude Code tries a failed request of the turn under way again: one row for the turn,
    /// made at the first attempt and updated in place by each after it (`error`, which attempt
    /// comes next and in how long, counted from the row's `at_ms`), gone once the turn streams
    /// again ([`Self::retried`]) and left as the turn's error when the attempts run out
    /// ([`Self::ended`]).
    pub fn retrying(&mut self, error: &str, retry: Retry, now: WallMs) -> Vec<Out> {
        let turn = self.threads.get(&conv::ThreadId::Main).map_or(TurnId::BEFORE, |m| m.turn);
        let had = self.retry.take().filter(|item| item.turn == turn);
        let id = had.as_ref().map_or_else(
            || ItemId(format!("{}-retry-{}", Notice::API_ERROR, now.as_millis())),
            |item| item.id.clone(),
        );
        let notice = Notice {
            kind: Notice::API_ERROR.to_owned(),
            text: Clipped::whole(error),
            retry: Some(retry),
        };
        let item = Item { id, turn, at_ms: now, body: ItemBody::Notice(notice) };
        self.retry = Some(item.clone());
        let action =
            if had.is_some() { Action::ItemUpdated(item) } else { Action::ItemStarted(item) };
        self.push(self.meta.id, action);
        self.drain()
    }

    /// The request being tried again went through: its row goes.
    pub fn retried(&mut self) -> Vec<Out> {
        if let Some(item) = self.retry.take() {
            self.push(self.meta.id, Action::ItemRemoved { item: item.id });
        }
        self.drain()
    }

    /// A notice in the main thread's turn under way, or its last, at `now`: what Slopty did
    /// there that the transcript does not say (a setting Claude Code refused).
    pub fn notice(&mut self, kind: &str, text: &str, now: WallMs) -> Vec<Out> {
        let turn = self.threads.get(&conv::ThreadId::Main).map_or(TurnId::BEFORE, |m| m.turn);
        self.notices = self.notices.saturating_add(1);
        let item = Item {
            id: ItemId(format!("{kind}-{}-{}", now.as_millis(), self.notices)),
            turn,
            at_ms: now,
            body: ItemBody::Notice(Notice::new(kind, Clipped::whole(text))),
        };
        self.push(self.meta.id, Action::ItemCompleted(item));
        self.drain()
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

    /// What the decoder's `changes` (and the background commands' `outputs`) make of the
    /// threads.
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
        self.settle_subagents();
        self.drain()
    }

    /// A block the model is writing, heard at `wall`: begun, grown or let go.
    pub fn live(&mut self, change: Live, wall: WallMs) -> Vec<Out> {
        self.wall = wall;
        match change {
            Live::Start { thread, id, kind } => self.live_start(&thread, id, &kind),
            Live::Append { id, text } => {
                if let Some(held) = self.deferred.iter_mut().find(|d| d.id == id) {
                    held.text.push_str(&text);
                    return self.drain();
                }
                if let Some(p) = self.live.get(&id) {
                    let part = if p.tool { PartKey::Input } else { PartKey::Body };
                    let (thread, item) = (p.thread.clone(), p.item.clone());
                    let thread = self.ensure(&thread);
                    self.push(thread, Action::Append { item, part, text });
                }
            }
            Live::Clear { id } => self.cleared(&[Live::Clear { id }]),
        }
        self.drain()
    }

    /// Live block `id` is settled by the entry `entry` that comes next: a text's or a
    /// thinking's entry takes the block's item, where it stands, and a tool call's has its id
    /// already.
    pub fn settle(&mut self, id: &LiveId, entry: &str) {
        self.deferred.retain(|d| d.id != *id);
        let Some(p) = self.live.remove(id) else { return };
        if !p.tool
            && let Some(mapped) = self.threads.get_mut(&p.thread)
        {
            mapped.shown_as.insert(entry.to_owned(), p.item);
        }
    }

    /// What an agent at rest waits on: the tasks left running and the scheduled prompts. When every
    /// one is a command the session's own transcript shows running in the background (a dev
    /// server, say), it waits on [`Wait::COMMAND`], named by those commands; a subagent, a
    /// monitor or a scheduled prompt among them is a task, named by the engine's words for a
    /// lone one, else counted.
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
        self.reshow();
    }

    /// The person's and the project's own commands in this session's folder
    /// (`crate::commands::custom`): the menu until Claude Code lists its own, and where each of
    /// those comes from.
    pub fn commands(&mut self, custom: &[conv::SlashCommand]) -> Vec<Out> {
        custom.clone_into(&mut self.custom);
        self.tell_commands();
        self.drain()
    }

    /// Claude Code's own list of its commands and skills: the menu
    /// ([`crate::commands::listed`]).
    pub fn listed(&mut self, commands: Vec<crate::commands::Listed>) -> Vec<Out> {
        if self.listed.as_ref() != Some(&commands) {
            self.listed = Some(commands);
            self.tell_commands();
        }
        self.drain()
    }

    /// The models Claude Code offers, in its menu's order.
    pub fn models(&mut self, models: Vec<Model>) -> Vec<Out> {
        if !models.is_empty() && models != self.meta.models {
            self.meta.models = models;
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// What its permission modes and efforts are, as offered.
    pub fn offers(&mut self, modes: Vec<thread::Mode>, efforts: Vec<thread::Effort>) -> Vec<Out> {
        if modes != self.meta.modes || efforts != self.meta.efforts {
            self.meta.modes = modes;
            self.meta.efforts = efforts;
            self.push(self.meta.id, Action::Meta(Box::new(self.meta.clone())));
        }
        self.drain()
    }

    /// Tell the thread its menu, when it changed.
    fn tell_commands(&mut self) {
        let agent = self.listed.as_deref();
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
                skill: c.skill,
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

    /// The Claude Code that runs the session now started at `at`, as its process said. A
    /// subagent runs only inside the process that started it, in the foreground or the
    /// background, and a session taken up again (`--resume`) brings none back: one started
    /// before then that never said how it ended ended with the process before.
    pub fn started(&mut self, at: WallMs) -> Vec<Out> {
        if self.started != Some(at) {
            self.started = Some(at);
            self.settle_subagents();
        }
        self.drain()
    }

    /// The meters as they stand now.
    #[must_use]
    pub const fn meters_now(&self) -> &Meters {
        &self.meters
    }

    /// The meters are `meters` now.
    pub fn meters(&mut self, meters: Meters) -> Vec<Out> {
        if meters != self.meters {
            self.meters = meters;
            self.push(self.meta.id, Action::MetersSet(self.meters.clone()));
        }
        self.drain()
    }

    /// A permission prompt held for the session, or settled.
    pub fn permission(&mut self, event: &PermissionEvent) -> Vec<Out> {
        let action = match event {
            PermissionEvent::Asked(prompt) => {
                let request = request(prompt);
                self.open.push(request.clone());
                if let Some(call) = &request.item {
                    self.call_state(call, &ToolState::Pending { ask: request.id.clone() });
                }
                Action::RequestOpened(Box::new(request))
            }
            PermissionEvent::Settled { ask, outcome, .. } => {
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
                        RequestState::Answered { by: by.clone(), choice: choice_of(verdict) }
                    }
                    Settled::Withdrawn => RequestState::Withdrawn,
                };
                Action::RequestResolved { id: AskId(id), state }
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
        let spawn = self.spawned.get(agent);
        let parent = spawn.map(|spawn| Link {
            thread: self.threads.get(&spawn.thread).map_or(self.meta.id, |m| m.id),
            item: ItemId(spawn.call.clone()),
        });
        ThreadMeta {
            id,
            native: agent.to_owned(),
            title: spawn.map(|spawn| spawn.title.clone()).unwrap_or_default(),
            parent,
            origin: ThreadMeta::SUBAGENT.to_owned(),
            // A subagent's run is stopped on its own where its session's is (`stop_task`).
            caps: self
                .meta
                .caps
                .iter()
                .filter(|c| **c == Cap::named(Cap::INTERRUPT))
                .cloned()
                .collect(),
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
    }

    /// What the main thread holds that its transcript does not: told again after it began
    /// anew.
    fn again(&mut self) {
        let mut actions: Vec<Action> =
            self.shown(self.status.clone()).into_iter().map(Action::Status).collect();
        if self.meters != Meters::default() {
            actions.push(Action::MetersSet(self.meters.clone()));
        }
        actions.extend(self.open.iter().cloned().map(|r| Action::RequestOpened(Box::new(r))));
        if !self.commands.is_empty() {
            actions.push(Action::CommandsSet(self.commands.clone()));
        }
        for action in actions {
            self.push(self.meta.id, action);
        }
    }

    fn entry(&mut self, thread: &conv::ThreadId, entry: &conv::Entry) {
        self.last_at = self.last_at.max(entry.at_ms);
        if let Body::Tool(call) = &entry.body
            && let conv::ToolDetail::Agent(agent) = &call.detail
            && let Some(agent_id) = &agent.agent_id
        {
            self.spawn(agent_id, Spawn::of(thread, entry, call, agent));
        }
        let id = self.ensure(thread);
        let body = self.body(thread, entry);
        if id == self.meta.id
            && self.meta.title.is_empty()
            && let Body::Prompt(prompt) = &entry.body
            && prompt.command.is_none()
        {
            self.meta.title = title_of(&prompt.text.text);
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
        let shown_as = mapped.shown_as.get(&entry.id).cloned();
        let item_id = shown_as.unwrap_or_else(|| ItemId(entry.id.clone()));
        let mut item = Item { id: item_id, turn, at_ms: entry.at_ms, body };
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
        let mut tasks_changed = mapped.set_task(&entry.id, task);
        if let Body::Task(note) = &entry.body {
            tasks_changed |= mapped.note_task(note, entry.at_ms);
        }
        if tasks_changed {
            actions.push(Action::TasksSet(mapped.tasks.clone()));
        }
        let open = matches!(&item.body, ItemBody::Tool(call) if !call.state.is_final());
        if open {
            mapped.calls.insert(entry.id.clone(), item.clone());
        } else {
            mapped.calls.remove(&entry.id);
        }
        let item_id = item.id.clone();
        actions.push(if open { Action::ItemUpdated(item) } else { Action::ItemCompleted(item) });
        // What the TUI says of the call beyond its kind: its header, its line under it.
        if let Body::Tool(call) = &entry.body {
            let worded = (call.title.clone(), call.result.as_ref().and_then(|r| r.said.clone()));
            let had = mapped.worded.get(&entry.id);
            if (worded.0.is_some() || worded.1.is_some()) && had != Some(&worded) {
                mapped.worded.insert(entry.id.clone(), worded.clone());
                let note = thread::CallNote {
                    at_ms: entry.at_ms,
                    title: worded.0,
                    said: worded.1,
                    ..thread::CallNote::default()
                };
                actions.push(Action::CallNoted { item: item_id, note });
            }
        }
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
        if opened {
            self.place_deferred(thread);
        }
        self.rewait();
        // A new prompt leaves the failure behind: its own turn's end says how it went.
        if began {
            self.failed = false;
        }
    }

    /// The `Agent` call that started subagent `agent`, read again. A subagent whose own
    /// transcript came before its call named it (a call in the foreground names its subagent
    /// only with its result) was begun with no parent and no title: it is told them now.
    fn spawn(&mut self, agent: &str, spawn: Spawn) {
        let named = |s: &Spawn| (s.thread.clone(), s.call.clone(), s.title.clone());
        let renamed = self.spawned.get(agent).is_none_or(|was| named(was) != named(&spawn));
        self.spawned.insert(agent.to_owned(), spawn);
        let Some(id) = self.threads.get(&conv::ThreadId::Agent(agent.to_owned())).map(|m| m.id)
        else {
            return;
        };
        if renamed {
            let meta = self.subagent_meta(id, agent);
            self.push(id, Action::Meta(Box::new(meta)));
        }
    }

    /// Each subagent's thread says where its run stands: working while its call runs, and once
    /// the call that started it ended (or Claude Code said its run in the background did), its
    /// open turn ended and the thread exited as the run ended, done, failed or stopped. One the
    /// Claude Code before this one started, which never said how it ended, stopped with that
    /// process ([`Self::started`]). Nothing takes a subagent up again alone, so it is not
    /// resumable.
    fn settle_subagents(&mut self) {
        let agents: Vec<String> = self
            .threads
            .keys()
            .filter_map(|thread| match thread {
                conv::ThreadId::Agent(agent) => Some(agent.clone()),
                conv::ThreadId::Main => None,
            })
            .collect();
        for agent in agents {
            let spawn = self.spawned.get(&agent);
            let (wall, started) = (self.wall, self.started);
            let Some(mapped) = self.threads.get_mut(&conv::ThreadId::Agent(agent)) else {
                continue;
            };
            let began = spawn.map(|spawn| spawn.at_ms).or_else(|| {
                mapped.turns.values().map(|t| t.started_ms).filter(|at| !at.is_zero()).min()
            });
            let before = started.filter(|started| began.is_some_and(|began| began < *started));
            let end = spawn
                .and_then(|spawn| spawn.end.clone())
                .or_else(|| before.map(|started| (SubagentEnd::Stopped, started)));
            let mut actions = Vec::new();
            let waiting = spawn.is_some_and(|spawn| spawn.waiting);
            let status = match end {
                None if waiting => Status {
                    phase: Phase::Waiting,
                    wait: Some(Wait { kind: Wait::TASK.to_owned(), text: WAITING_TEXT.to_owned() }),
                    liveness: Liveness::Live,
                    since_ms: wall,
                },
                None => {
                    let began = mapped.turns.values().map(|t| t.started_ms).min();
                    let since_ms = began.unwrap_or(wall);
                    Status { phase: Phase::Working, wait: None, liveness: Liveness::Live, since_ms }
                }
                Some((end, at_ms)) => {
                    let (state, phase) = end.ended();
                    let turn = mapped.turn;
                    if turn != TurnId::BEFORE && mapped.ended.insert(turn) {
                        let usage = mapped.turns.get(&turn).map(|t| t.usage.clone());
                        if let Some(t) = mapped.turns.get_mut(&turn) {
                            t.state = state.clone();
                            t.ended_ms = Some(at_ms);
                        }
                        let usage = usage.unwrap_or_default();
                        actions.push(Action::TurnEnded { turn, state, usage, ended_ms: at_ms });
                    }
                    let liveness = Liveness::Exited { resumable: false };
                    Status { phase, wait: None, liveness, since_ms: at_ms }
                }
            };
            let same = |told: &Status| {
                told.phase == status.phase
                    && told.liveness == status.liveness
                    && told.wait == status.wait
            };
            if !mapped.told.as_ref().is_some_and(same) {
                mapped.told = Some(status.clone());
                actions.push(Action::Status(status));
            }
            let id = mapped.id;
            for action in actions {
                self.push(id, action);
            }
        }
    }

    fn remove(&mut self, thread: &conv::ThreadId, entry: &str) {
        let id = self.ensure(thread);
        let Some(mapped) = self.threads.get_mut(thread) else { return };
        let item = mapped.shown_as.remove(entry).unwrap_or_else(|| ItemId(entry.to_owned()));
        let mut actions = vec![Action::ItemRemoved { item }];
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
            let items = &mapped.items;
            mapped.shown_as.retain(|entry, _| items.contains_key(entry));
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
        let state = mapped.by_engine.get(&turn).cloned().unwrap_or(state);
        if let Some(ended_ms) = record.ended_ms {
            updated.state = state.clone();
            // A turn the engine ended ends when it said; one the records alone end, when the
            // last of them says.
            updated.ended_ms = if mapped.by_engine.contains_key(&turn) {
                updated.ended_ms.or(Some(ended_ms))
            } else {
                Some(ended_ms)
            };
        } else if let Some(by_engine) = mapped.by_engine.get(&turn) {
            updated.state = by_engine.clone();
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
        self.reshow();
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
        let item = Item { id: item, turn, at_ms: self.wall, body };
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
                intent: self.intents.get(&entry.id).copied(),
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
                    NoteKind::Report => Notice::REPORT,
                    NoteKind::CommandError => Notice::COMMAND_ERROR,
                    NoteKind::Quiet => Notice::NOTE,
                }
                .to_owned(),
                text: clip(&note.text),
                retry: note.retry.map(|r| Retry {
                    attempt: r.attempt,
                    max: Some(r.max),
                    in_ms: Some(r.in_ms),
                }),
            }),
            Body::Task(note) => ItemBody::Task(Box::new(thread::TaskNote {
                task: note.task_id.clone(),
                kind: task_kind(&note.kind).to_owned(),
                status: note.status.clone(),
                title: note.title.clone(),
                summary: note.summary.clone(),
                body: note.body.as_ref().map(clip),
                call: note.call.clone().map(ItemId),
                usage: task_usage(note.usage),
                exit_code: note.exit_code,
                links: self.task_links.get(&note.task_id).cloned().unwrap_or_default(),
            })),
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
                        edits: b.edits.clone(),
                        interpretation: b.interpretation.clone(),
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
                // A tool whose descriptor names a UI resource is an MCP App: its page.
                let shown = match self.apps.get(&(m.server.clone(), m.tool.clone())) {
                    Some(app) => detail::ToolDetail::Page(slopty_proto::visual::PageDetail {
                        title: m.tool.clone(),
                        source: slopty_proto::visual::PageSource::App(
                            slopty_proto::visual::McpApp {
                                result: m.result.clone(),
                                ..app.clone()
                            },
                        ),
                    }),
                    None => detail::ToolDetail::Mcp(detail::McpDetail {
                        server: m.server.clone(),
                        tool: m.tool.clone(),
                        structured: m.structured.clone(),
                        links: m.links.clone(),
                    }),
                };
                (kind::MCP, format!("{}: {}", m.server, m.tool), Some(shown))
            }
            // A visual shows once its call worked: Slopty's server checked its script then.
            conv::ToolDetail::Shown(shown)
                if matches!(shown.as_ref(), detail::ToolDetail::Visual(_))
                    && call.result.as_ref().is_none_or(|r| r.status != ResultStatus::Ok) =>
            {
                let tool = slopty_proto::visual::SHOW_VISUAL;
                (
                    kind::MCP,
                    format!("{}: {tool}", crate::mcp::MCP_SERVER_NAME),
                    Some(detail::ToolDetail::Mcp(detail::McpDetail {
                        server: crate::mcp::MCP_SERVER_NAME.to_owned(),
                        tool: tool.to_owned(),
                        structured: None,
                        links: Vec::new(),
                    })),
                )
            }
            conv::ToolDetail::Shown(shown) => {
                let title = match shown.as_ref() {
                    detail::ToolDetail::Visual(v) => v.title.clone(),
                    detail::ToolDetail::Page(p) => p.title.clone(),
                    _ => call.name.clone(),
                };
                let title = if title.is_empty() { call.name.clone() } else { title };
                (kind::MCP, title, Some(self.rooted(shown)))
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
        "Bash" | "PowerShell" => kind::EXEC,
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
                conv::AgentRun::Waiting => BackgroundTask::WAITING,
                conv::AgentRun::Completed => BackgroundTask::COMPLETED,
                conv::AgentRun::Failed => BackgroundTask::FAILED,
                conv::AgentRun::Killed => BackgroundTask::KILLED,
            };
            let ended = a
                .duration_ms
                .filter(|_| state != BackgroundTask::RUNNING && state != BackgroundTask::WAITING)
                .map(|ms| WallMs::from_millis(entry.at_ms.as_millis().saturating_add(ms)));
            let id = a.agent_id.clone().unwrap_or_else(|| entry.id.clone());
            (id, BackgroundTask::AGENT, state, ended)
        }
        _ => return None,
    };
    Some(BackgroundTask {
        state: state.to_owned(),
        item: Some(item.id.clone()),
        output: mapped.output.clone().filter(|_| kind == BackgroundTask::SHELL),
        ended_ms,
        ..BackgroundTask::new(&id, kind, &mapped.title, entry.at_ms)
    })
}

/// `task`, as the transcript says it, with what the engine said of it live (`had`) where the
/// transcript says nothing: its figures, its step, its kind of agent, its links.
fn merge_live(had: &BackgroundTask, task: &mut BackgroundTask) {
    task.agent_type = task.agent_type.take().or_else(|| had.agent_type.clone());
    task.step = task.step.take().or_else(|| had.step.clone());
    task.usage = task.usage.or(had.usage);
    task.parent = task.parent.take().or_else(|| had.parent.clone());
    task.child = task.child.or(had.child);
    task.outcome = task.outcome.take().or_else(|| had.outcome.clone());
    task.timeout_ms = task.timeout_ms.or(had.timeout_ms);
    task.ambient |= had.ambient;
    task.awaited |= had.awaited;
    task.persistent |= had.persistent;
    if had.kind != BackgroundTask::SHELL && task.kind == BackgroundTask::SHELL {
        task.kind.clone_from(&had.kind);
    }
    // The call's own id stands in for a task id the transcript does not give yet.
    let standing_in = task.item.as_ref().is_some_and(|item| item.0 == task.id);
    if standing_in && !had.id.is_empty() && task.id != had.id && task.item == had.item {
        task.id.clone_from(&had.id);
    }
}

/// A task notice's kind in the thread's words ([`BackgroundTask::kind`]).
fn task_kind(kind: &str) -> &str {
    match kind {
        "agent" => BackgroundTask::AGENT,
        "shell" => BackgroundTask::SHELL,
        "monitor" => BackgroundTask::MONITOR,
        "remote" => BackgroundTask::REMOTE,
        other => other,
    }
}

/// A notice's figures, when it gives any.
fn task_usage(usage: (Option<u64>, Option<u64>, Option<u64>)) -> Option<thread::TaskUsage> {
    let (tokens, tool_uses, duration_ms) = usage;
    (tokens.is_some() || tool_uses.is_some() || duration_ms.is_some()).then(|| thread::TaskUsage {
        tokens: tokens.unwrap_or(0),
        tool_uses: tool_uses.unwrap_or(0),
        duration_ms: duration_ms.unwrap_or(0),
    })
}

/// What a notice's words say of how its task ended, past the task's quoted name: "finished",
/// "is waiting on its own background work", "failed: out of memory".
fn outcome_of(summary: &str) -> String {
    summary
        .split_once('"')
        .and_then(|(_, rest)| rest.split_once('"'))
        .map_or(summary, |(_, after)| after)
        .trim()
        .to_owned()
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
                        preview: o.preview.clone(),
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
    let (kind, title, options, questions, proposed) = match &prompt.detail {
        conv::ToolDetail::Question(q) => {
            let title =
                q.questions.first().map_or_else(|| "A question".to_owned(), |q| q.text.clone());
            // "Chat about this": none of the answers, the questions talked over first.
            let clarify = choice(CLARIFY, "Chat about this", Effect::Deny, None, false);
            (Request::QUESTION, title, vec![deny, clarify], question(q).questions, None)
        }
        // Claude Code's own plan dialog, in its words: approve with a clear context, in the mode
        // its request suggests (carried back as "Always allow" carries its suggestions), with
        // edits still asked about, or keep planning.
        conv::ToolDetail::Plan { .. } => {
            let options = cards::plan_options(prompt, grants(prompt));
            (Request::PLAN, "Ready to code?".to_owned(), options, Vec::new(), None)
        }
        _ if prompt.tool == "EnterPlanMode" => {
            let (title, _) = cards::approval(prompt);
            (Request::APPROVAL, title, cards::enter_plan_options(), Vec::new(), None)
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
            // A request Claude Code would start on "No" (`default_to_no`) lists its deny first,
            // where the card's focus starts.
            if prompt.default_to_no {
                options.rotate_right(2);
            }
            let proposed = match detail {
                conv::ToolDetail::Edit(e) => Some(e.patch.clone()),
                conv::ToolDetail::Write(w) => Some(w.patch.clone()),
                _ => None,
            };
            (Request::APPROVAL, cards::approval(prompt).0, options, Vec::new(), proposed)
        }
    };
    // What the card asks under its title, in the TUI's words; a plan is its own text.
    let text = match &prompt.detail {
        conv::ToolDetail::Plan { plan } => Some(clipped(&thread, plan)),
        conv::ToolDetail::Bash(b) if prompt.input.is_null() => Some(clipped(&thread, &b.command)),
        _ => cards::approval(prompt).1.map(|text| Clipped::whole(&text)),
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
        reason: prompt.reason.clone(),
        state: RequestState::Open,
        opened_ms: prompt.asked_ms,
        until_ms: None,
        needs_presence: prompt.needs_presence,
    }
}

/// The fact that names the branch checked out where the agent works, as Codex's threads name it.
pub const BRANCH_FACT: &str = "branch";

/// The branch the last of `records` that names one was written on (`gitBranch`).
#[must_use]
pub fn branch_in(records: &[serde_json::Value]) -> Option<&str> {
    records
        .iter()
        .rev()
        .find_map(|r| r.get("gitBranch").and_then(serde_json::Value::as_str))
        .filter(|b| !b.is_empty())
}

/// The main thread of a Claude Code session rebuilt from its `records` alone.
///
/// The records are its main transcript's, then each subagent's (whose records name their agent),
/// as Claude Code wrote them. Nothing runs while a session is read again: a turn the records
/// leave open ends there.
#[must_use]
pub fn replay(records: &[serde_json::Value]) -> ThreadState {
    let field = |key: &str| {
        records
            .iter()
            .find_map(|r| r.get(key).and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .to_owned()
    };
    let mut history =
        History::new(&field("sessionId"), &field("version"), &field("cwd"), &[], WallMs::ZERO);
    let main = history.main();
    let mut decoder = conv::Conversation::default();
    let mut outs = history.drain();
    for record in records {
        let changes = decoder.ingest(&conv::ThreadId::Main, record);
        outs.extend(history.transcript(&changes, &[]));
    }
    if let Some(branch) = branch_in(records) {
        outs.extend(history.fact(BRANCH_FACT, Some(branch)));
    }
    outs.extend(history.rest(WallMs::ZERO));
    let mut state = ThreadState::new(history.meta().clone());
    for out in outs {
        match out {
            Out::Begin(meta) if meta.id == main => state = ThreadState::new(*meta),
            Out::Actions(id, actions) if id == main => {
                for action in &actions {
                    state.apply(action);
                }
            }
            Out::Begin(_) | Out::Actions(..) => {}
        }
    }
    state
}

mod cards;

#[cfg(test)]
mod tests;
