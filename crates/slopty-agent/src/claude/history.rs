//! A Claude Code session's record mapped onto the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Its conversation is read from the session's own transcript as the TUI writes it, through the
//! transcript decoder ([`crate::conversation`]).
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
//!   Claude Code's `Stop` ([`History::ended`]) or the transcript's turn record ends it, and a
//!   prompt that comes while a turn is open ends that one. A branch (a rewind, an edited prompt)
//!   truncates the thread back to before the dropped prompt.
//! - **Items.** An entry keeps its id. A tool call's kind comes from the decoder's typed detail,
//!   not from Claude Code's tool names, and its title is worded here.
//! - **Status.** The phase is told from outside ([`History::status`]): the session's hooks, or its
//!   transcript when it has none. A dialog the TUI holds is a wait on the person, never a request
//!   answered here: the person answers it in the terminal.

use std::collections::{BTreeMap, HashMap, HashSet};

use slopty_core::WallMs;
use slopty_proto::thread::{
    self, Action, AgentId, BackgroundTask, Changed, Clipped, Compaction, ContentRef, Draft, Drive,
    Item, ItemBody, ItemId, Link, Liveness, Notice, Phase, Plan, Retry, Status, Step, ThreadId,
    ThreadMeta, ThreadState, ToolCall, ToolState, Turn, TurnId, TurnState, Usage, UserMessage,
    Wait, detail, kind,
};

use crate::conversation::{self as conv, Body, Change, NoteKind, ResultStatus, TextRef};
use crate::title::title_of;

/// What the threads of a session need done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Host this thread from nothing: a new one, or one held from before (a worker restart,
    /// a transcript read again), whose log starts over.
    Begin(Box<ThreadMeta>),
    /// Apply actions to a thread.
    Actions(ThreadId, Vec<Action>),
}

/// How Claude Code ended a turn ([`History::ended`]).
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

/// The words a thread waits with while it is compacted.
pub const COMPACTING_TEXT: &str = "Compacting the conversation";

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

/// The choice that answers an agent's questions with none of their answers, to talk them over
/// first (the TUI's "Chat about this").
pub const CLARIFY: &str = "clarify";

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
    /// Turns Claude Code's hooks ended, as they ended them: the transcript's record of one
    /// changes it no more.
    by_engine: HashMap<TurnId, TurnState>,
    /// Each turn as last sent.
    turns: HashMap<TurnId, Turn>,
    /// Background commands as last sent, for their output to land on.
    background: HashMap<String, Item>,
    /// The work left running in the background, by the call that started it, in the order
    /// the calls came, as last sent.
    tasks: Vec<BackgroundTask>,
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
            tasks: Vec::new(),
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

/// End each of `tasks` still at work at `at`, as `state` says, but one waiting on its own work
/// (a subagent that reported), which completed; whether any was.
fn end_tasks(tasks: &mut [BackgroundTask], at: WallMs, state: &str) -> bool {
    let mut ended = false;
    for task in tasks.iter_mut().filter(|task| task.is_live()) {
        let state =
            if task.state == BackgroundTask::WAITING { BackgroundTask::COMPLETED } else { state };
        state.clone_into(&mut task.state);
        task.ended_ms = task.ended_ms.or(Some(at));
        ended = true;
    }
    ended
}

/// One Claude Code session as threads.
#[derive(Debug)]
pub struct History {
    meta: ThreadMeta,
    threads: HashMap<conv::ThreadId, Mapped>,
    /// Each subagent's call, by the subagent's id.
    spawned: HashMap<String, Spawn>,
    /// The status last told: a thread begun anew from the transcript gets it again, since the
    /// transcript has none.
    status: Option<Status>,
    out: Vec<Out>,
    /// The main thread's last turn ended failed, so its agent's done is a failure.
    failed: bool,
    /// When the Claude Code that ran the session last went ([`Self::gone`]).
    went: Option<WallMs>,
    /// No Claude Code runs the session: the last one ended by then ([`Self::rest`]).
    rested: Option<WallMs>,
    /// Its context is being compacted ([`Wait::COMPACTING`]).
    compacting: bool,
    /// When the latest entry was made.
    last_at: WallMs,
    /// The TUI's prompt box as last told ([`Self::draft`]).
    draft: Option<Draft>,
    /// The prompts queued in the TUI, as its transcript says them.
    queue: Vec<conv::Queued>,
    /// When the TUI that runs the session now began ([`Self::run_began`]): a prompt queued
    /// before was another run's, and went with it.
    queue_since: Option<WallMs>,
    /// The queue as last told.
    queued: Vec<String>,
}

impl History {
    /// Session `native` of Claude Code `version` in `cwd`, as Slopty reads it. The first outs it
    /// gives, from whichever call, begin its thread.
    #[must_use]
    pub fn new(native: &str, version: &str, cwd: &str, now: WallMs) -> Self {
        let meta = ThreadMeta {
            id: thread_of(native),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: version.to_owned(),
            native: native.to_owned(),
            cwd: cwd.to_owned(),
            title: String::new(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            drive: Drive::named(Drive::OBSERVED),
            facts: BTreeMap::new(),
            created_ms: now,
        };
        Self::of(meta)
    }

    /// The thread `meta` as it stands, read again from nothing: what it is and who holds it are
    /// kept, what it holds is told again by the records that follow.
    #[must_use]
    pub fn of(meta: ThreadMeta) -> Self {
        let mut threads = HashMap::new();
        threads.insert(conv::ThreadId::Main, Mapped::new(meta.id));
        let mut history = Self {
            out: Vec::new(),
            meta,
            threads,
            spawned: HashMap::new(),
            status: None,
            failed: false,
            went: None,
            rested: None,
            compacting: false,
            last_at: WallMs::ZERO,
            draft: None,
            queue: Vec::new(),
            queue_since: None,
            queued: Vec::new(),
        };
        history.out.push(Out::Begin(Box::new(history.meta.clone())));
        history
    }

    /// The main thread's turn under way, or its last: what a session with no hooks takes its
    /// phase from.
    #[must_use]
    pub fn last_turn(&self) -> Option<&Turn> {
        let main = self.threads.get(&conv::ThreadId::Main)?;
        main.turns.get(&main.turn)
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

    /// `status` as the thread shows it: compacting while its context is compacted.
    fn shown(&self, status: Option<Status>) -> Option<Status> {
        let mut status = status?;
        if status.liveness == Liveness::Live && self.compacting {
            status.phase = Phase::Working;
            let text = COMPACTING_TEXT.to_owned();
            status.wait = Some(Wait { kind: Wait::COMPACTING.to_owned(), text });
        }
        Some(status)
    }

    /// Tell the thread its status again as [`Self::shown`] shows it, after an overlay moved.
    fn reshow(&mut self) {
        if let Some(shown) = self.shown(self.status.clone()) {
            self.push(self.meta.id, Action::Status(shown));
        }
    }

    /// Claude Code is at `phase` since `since_ms`, as its hooks or its transcript say, alive or
    /// not; what it waits on, if anything, is `wait` (a permission, a question), else its
    /// compaction.
    pub fn status(
        &mut self,
        phase: Phase,
        wait: Option<Wait>,
        liveness: Liveness,
        since_ms: WallMs,
    ) -> Vec<Out> {
        let phase = if phase == Phase::Done && self.failed { Phase::Failed } else { phase };
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

    /// Its context is being compacted, or was ([`Wait::COMPACTING`]).
    pub fn compacting(&mut self, on: bool) -> Vec<Out> {
        if self.compacting != on {
            self.compacting = on;
            self.reshow();
        }
        self.drain()
    }

    /// Claude Code said the main loop's turn under way ended (its `Stop` hook), as `end` says,
    /// at `now`: the turn open ends so, and a transcript record of it that comes after changes
    /// that no more. The thread rests: stopped after an interruption, failed after an error,
    /// else done.
    pub fn ended(&mut self, end: &End, now: WallMs) -> Vec<Out> {
        let phase = match end {
            End::Interrupted => Phase::Stopped,
            End::Failed(_) => Phase::Failed,
            End::Complete => Phase::Done,
        };
        let id = self.meta.id;
        let mut actions = Vec::new();
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
                                    None,
                                )
                            },
                            |(stop, error)| (error.clone(), stop.until_ms),
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
            self.reshow();
        }
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
        // Nothing it started runs on without it: its subagents and the work left in the
        // background ended with the last Claude Code that ran it.
        let ended = if self.last_at.is_zero() { at } else { self.last_at };
        self.rested = Some(ended);
        self.settle_subagents();
        if let Some(main) = self.threads.get_mut(&conv::ThreadId::Main)
            && end_tasks(&mut main.tasks, ended, BackgroundTask::KILLED)
        {
            let tasks = main.tasks.clone();
            self.push(self.meta.id, Action::TasksSet(tasks));
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
        self.went = Some(now);
        // The box and the queue are the TUI's, and went with it.
        if self.draft.take().is_some() {
            self.push(self.meta.id, Action::DraftSet(None));
        }
        self.queue_since = Some(now);
        self.tell_queue();
        self.settle_subagents();
        self.drain()
    }

    /// What the TUI's prompt box holds now ([`slopty_proto::thread::Draft`]), `None` for
    /// nothing: told when its words or its pictures changed.
    pub fn draft(&mut self, draft: Option<Draft>) -> Vec<Out> {
        let same = match (&self.draft, &draft) {
            (Some(had), Some(now)) => had.text == now.text && had.images == now.images,
            (had, now) => had.is_none() && now.is_none(),
        };
        if !same {
            self.draft.clone_from(&draft);
            self.push(self.meta.id, Action::DraftSet(draft));
        }
        self.drain()
    }

    /// The TUI that runs the session now began at `at`: a prompt the transcript says was
    /// queued before then was another run's, which took it when it ended.
    pub fn run_began(&mut self, at: WallMs) -> Vec<Out> {
        self.queue_since = Some(at);
        self.tell_queue();
        self.drain()
    }

    /// Tell the prompts queued in this run, when they are not what was told.
    fn tell_queue(&mut self) {
        let since = self.queue_since.unwrap_or(WallMs::ZERO);
        let queued: Vec<String> = self
            .queue
            .iter()
            .filter(|q| q.at_ms >= since && !q.text.trim().is_empty())
            .map(|q| q.text.clone())
            .collect();
        if queued != self.queued {
            self.queued.clone_from(&queued);
            self.push(self.meta.id, Action::QueuedSet(queued));
        }
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
                Change::Queued(queued) => {
                    self.queue.clone_from(queued);
                    self.tell_queue();
                }
            }
        }
        for output in outputs {
            self.output(output);
        }
        self.settle_subagents();
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

    /// Subagent `agent`'s thread `id`: its session's agent, folder and drive, hung off the call
    /// that started it. None of the session's facts are its own: the terminal, the seat and the
    /// orchestrating parent are the session's, which a client finds through [`ThreadMeta::parent`].
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
            facts: BTreeMap::new(),
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
            // What the main thread holds that its transcript does not, its status, is told
            // again once it began anew.
            if thread == conv::ThreadId::Main {
                self.failed = false;
                self.reshow();
                if let Some(draft) = &self.draft {
                    self.push(id, Action::DraftSet(Some(draft.clone())));
                }
                // The queue comes again with the records, into a thread that holds none.
                self.queue.clear();
                self.queued.clear();
            }
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
        let mut began = false;
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
                };
                mapped.turns.insert(turn, started.clone());
                actions.push(Action::TurnStarted(started));
                began = *thread == conv::ThreadId::Main;
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
        let item = Item { id: ItemId(entry.id.clone()), turn, at_ms: entry.at_ms, body };
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
    /// open turn ended and the thread exited as the run ended, done, failed or stopped. One begun
    /// before its Claude Code went ([`Self::gone`]), which never said how it ended, stopped with
    /// that process. Nothing takes a subagent up again alone, so it is not resumable.
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
            let (wall, started, rested) = (self.last_at, self.went, self.rested);
            let Some(mapped) = self.threads.get_mut(&conv::ThreadId::Agent(agent)) else {
                continue;
            };
            let began = spawn.map(|spawn| spawn.at_ms).or_else(|| {
                mapped.turns.values().map(|t| t.started_ms).filter(|at| !at.is_zero()).min()
            });
            let before = started.filter(|started| began.is_some_and(|began| began < *started));
            let waiting = spawn.is_some_and(|spawn| spawn.waiting);
            // At rest, one waiting on its own work had reported: that work ended with it.
            let rested =
                rested.filter(|rested| began.is_none_or(|began| began <= *rested)).map(|rested| {
                    let end = if waiting { SubagentEnd::Completed } else { SubagentEnd::Stopped };
                    (end, rested)
                });
            let end = spawn
                .and_then(|spawn| spawn.end.clone())
                .or_else(|| before.map(|started| (SubagentEnd::Stopped, started)))
                .or(rested);
            let mut actions = Vec::new();
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
                    // Its own work in the background ran inside it, and ended with it.
                    let tasks_end = if end == SubagentEnd::Completed {
                        BackgroundTask::COMPLETED
                    } else {
                        BackgroundTask::KILLED
                    };
                    if end_tasks(&mut mapped.tasks, at_ms, tasks_end) {
                        actions.push(Action::TasksSet(mapped.tasks.clone()));
                    }
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
        let mut actions = vec![Action::ItemRemoved { item: ItemId(entry.to_owned()) }];
        mapped.items.remove(entry);
        mapped.background.remove(entry);
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
    }

    fn turn(&mut self, thread: &conv::ThreadId, record: &conv::Turn) {
        let id = self.ensure(thread);
        let Some(mapped) = self.threads.get_mut(thread) else { return };
        let Some(&turn) = mapped.prompts.get(&record.prompt) else { return };
        let state = match (record.ended_ms, mapped.stopped.get(&turn)) {
            (None, _) => TurnState::Active,
            (Some(_), _) if mapped.interrupted.contains(&turn) => TurnState::Interrupted,
            (Some(_), Some((stop, error))) => {
                TurnState::Failed { error: error.clone(), until_ms: stop.until_ms }
            }
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

    fn body(&self, thread: &conv::ThreadId, entry: &conv::Entry) -> ItemBody {
        let clip = |c: &conv::Clipped| clipped(thread, c);
        match &entry.body {
            Body::Prompt(prompt) => ItemBody::User(UserMessage {
                text: clip(&prompt.text),
                images: prompt.images.iter().map(|i| image(thread, i)).collect(),
                command: prompt.command.clone(),
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
                links: Vec::new(),
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
                let shown = detail::ToolDetail::Mcp(detail::McpDetail {
                    server: m.server.clone(),
                    tool: m.tool.clone(),
                    structured: m.structured.clone(),
                    links: m.links.clone(),
                });
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

/// Claude Code's own titles for the session its `records` hold, each the last one written.
///
/// The name the person gave it (`custom-title`), and the title Claude Code made (`ai-title`),
/// as its resume picker reads them.
#[must_use]
pub fn titles_in(records: &[serde_json::Value]) -> (Option<&str>, Option<&str>) {
    let last = |kind: &str, key: &str| {
        records
            .iter()
            .rev()
            .filter(|r| r.get("type").and_then(serde_json::Value::as_str) == Some(kind))
            .find_map(|r| r.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|t| !t.is_empty())
    };
    (last("custom-title", "customTitle"), last("ai-title", "aiTitle"))
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
        History::new(&field("sessionId"), &field("version"), &field("cwd"), WallMs::ZERO);
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
    let (custom, ai) = titles_in(records);
    if let Some(title) = custom.or(ai) {
        outs.extend(history.named(title));
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

#[cfg(test)]
mod tests;
