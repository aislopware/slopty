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
//!   appended to as it grows, and removed once the transcript's entry settles it, the way the face
//!   shows it today ([`crate::live::Overlay`], which this keeps as one follower). A tool call's
//!   live block takes the call's own id, so the transcript's entry replaces it.
//! - **Requests.** A permission prompt the worker holds is a request with the answers Claude Code
//!   takes: allow, allow always (with what the suggestions grant), deny, and deny and stop.
//!   [`verdict`] maps a chosen answer back.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use slopty_core::{ClientId, SessionId, WallMs};
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_proto::conversation::{
    self as conv, Body, Change, Grant, Live, LiveId, LiveKind, NoteKind, PermissionEvent,
    PermissionPrompt, ResultStatus, Settled, TextRef, Verdict,
};
use slopty_proto::thread::{
    self, Action, AgentId, Answerer, AskId, Cap, Changed, Choice, Clipped, Compaction, ContentRef,
    Drive, Effect, Item, ItemBody, ItemId, Limit, Link, Liveness, Meters, Model, Notice, PartKey,
    Phase, Plan, Request, RequestState, Status, Step, ThreadId, ThreadMeta, ToolCall, ToolState,
    Turn, TurnId, TurnState, Usage, UserMessage, Wait, detail, kind,
};

use crate::live;

/// What the threads observed need done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Host this thread from nothing: a new one, or one held from before (a worker restart,
    /// a transcript that started over), whose log starts over.
    Begin(Box<ThreadMeta>),
    /// Apply actions to a thread.
    Actions(ThreadId, Vec<Action>),
}

/// The capabilities an observed Claude Code has through Slopty.
pub const CAPS: [&str; 8] = [
    Cap::APPROVALS,
    Cap::INTERRUPT,
    Cap::LIVE_TEXT,
    Cap::LIVE_TUI,
    Cap::QUEUE,
    Cap::SET_MODEL,
    Cap::SNAPSHOTS,
    Cap::STEER,
];

/// The models `/model` takes, by the aliases Claude Code resolves to its current ones: its
/// catalogue as the thread offers it.
pub const MODELS: [(&str, &str); 4] =
    [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku"), ("default", "Default")];

/// The thread of Claude Code session `native`.
#[must_use]
pub fn thread_of(native: &str) -> ThreadId {
    derived(&["claude-code session", native])
}

/// The thread of subagent `agent` in the session whose thread is `main`.
#[must_use]
pub fn subagent_of(main: ThreadId, agent: &str) -> ThreadId {
    derived(&["claude-code subagent", &main.to_string(), agent])
}

fn derived(parts: &[&str]) -> ThreadId {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(hasher.finalize().as_bytes().get(..16).unwrap_or(&[0; 16]));
    ThreadId::from_uuid(uuid::Builder::from_custom_bytes(bytes).into_uuid())
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
    /// Turns whose end was sent.
    ended: HashSet<TurnId>,
    /// Each turn as last sent.
    turns: HashMap<TurnId, Turn>,
    /// Background commands as last sent, for their output to land on.
    background: HashMap<String, Item>,
}

impl Mapped {
    fn new(id: ThreadId) -> Self {
        Self {
            id,
            turn: TurnId::BEFORE,
            prompts: HashMap::new(),
            items: HashMap::new(),
            interrupted: HashSet::new(),
            ended: HashSet::new(),
            turns: HashMap::new(),
            background: HashMap::new(),
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

/// One observed Claude Code session.
#[derive(Debug)]
pub struct Observed {
    meta: ThreadMeta,
    threads: HashMap<conv::ThreadId, Mapped>,
    /// Each subagent's call: the decoder's thread it is in, and its id.
    spawned: HashMap<String, (conv::ThreadId, String)>,
    overlay: live::Overlay,
    live: HashMap<LiveId, Provisional>,
    meters: Meters,
    /// The status last told, and the requests open: a thread begun anew from the transcript
    /// gets them again, since the transcript has neither.
    status: Option<Status>,
    open: Vec<Request>,
    out: Vec<Out>,
    /// The title is the session's own name, which its first prompt replaces.
    named: bool,
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
        let id = thread_of(native);
        let mut caps: Vec<Cap> = CAPS.iter().map(|c| Cap::named(c)).collect();
        caps.sort();
        let meta = ThreadMeta {
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
            meters: Meters::default(),
            status: None,
            open: Vec::new(),
            named: false,
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

    /// What the transcript's `changes` (and the background commands' `outputs`) make of the
    /// threads; live blocks the changes settle go after them.
    pub fn transcript(&mut self, changes: &[Change], outputs: &[conv::Output]) -> Vec<Out> {
        for change in changes {
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
        let (phase, wait, liveness) = match &event.status {
            AgentStatus::None => (Phase::Idle, None, Liveness::Exited { resumable: true }),
            AgentStatus::Idle => (Phase::Idle, None, Liveness::Live),
            AgentStatus::Working | AgentStatus::Tool { .. } => {
                (Phase::Working, None, Liveness::Live)
            }
            AgentStatus::Done | AgentStatus::Blocked(BlockReason::IdlePrompt) => {
                (Phase::Done, None, Liveness::Live)
            }
            AgentStatus::Blocked(reason) => {
                let (kind, text) = match reason {
                    BlockReason::Permission { tool } => {
                        ("permission", format!("Wants to use {tool}"))
                    }
                    BlockReason::Question => ("question", "Has a question".to_owned()),
                    BlockReason::Elicitation | BlockReason::IdlePrompt => {
                        ("input", "Asks for input".to_owned())
                    }
                };
                let text = event.detail.clone().unwrap_or(text);
                (Phase::NeedsYou, Some(Wait { kind: kind.to_owned(), text }), Liveness::Live)
            }
            AgentStatus::Waiting { tasks, crons } => {
                let text = match (tasks, crons) {
                    (0, n) => format!("{n} scheduled"),
                    (n, _) => format!("{n} in the background"),
                };
                (Phase::Waiting, Some(Wait { kind: "task".to_owned(), text }), Liveness::Live)
            }
        };
        let status = Status { phase, wait, liveness, since_ms: event.since_ms };
        self.status = Some(status.clone());
        self.push(self.meta.id, Action::Status(status));
        let mode = event.mode.as_ref().map(|m| m.name.clone());
        if mode.is_some() && mode != self.meters.mode {
            self.meters.mode = mode;
            self.push(self.meta.id, Action::MetersSet(self.meters.clone()));
        }
        self.drain()
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

    /// The status line's meters.
    pub fn meters(&mut self, meters: &conv::Meters) -> Vec<Out> {
        let limits = [("five-hour", meters.five_hour), ("seven-day", meters.seven_day)];
        let mapped = Meters {
            model: meters.model.clone(),
            model_id: meters.model_id.clone(),
            mode: self.meters.mode.clone(),
            context_tokens: meters
                .context_used_pct
                .zip(meters.context_window)
                .map(|(pct, window)| share(window, pct)),
            context_window: meters.context_window,
            cost_micro_usd: meters.cost_usd.map(|usd| share(100_000_000, usd)),
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
                let request = request(prompt);
                self.open.push(request.clone());
                Action::RequestOpened(Box::new(request))
            }
            PermissionEvent::Settled { ask, outcome, .. } => {
                let id = ask.to_string();
                self.open.retain(|r| r.id.0 != id);
                let state = match outcome {
                    Settled::Answered { verdict, by } => {
                        RequestState::Answered { by: answerer(*by), choice: choice_of(verdict) }
                    }
                    Settled::Released => RequestState::Released,
                    Settled::Withdrawn => RequestState::Withdrawn,
                };
                Action::RequestResolved { id: AskId(id), state }
            }
        };
        self.push(self.meta.id, action);
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
        let id = subagent_of(self.meta.id, &agent);
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
                self.again();
            }
        }
        self.live.clear();
        let _shown = self.overlay.clear_all();
    }

    /// What the main thread holds that its transcript does not: told again after it began
    /// anew.
    fn again(&mut self) {
        let mut actions: Vec<Action> = self.status.iter().cloned().map(Action::Status).collect();
        if self.meters != Meters::default() {
            actions.push(Action::MetersSet(self.meters.clone()));
        }
        actions.extend(self.open.iter().cloned().map(|r| Action::RequestOpened(Box::new(r))));
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
                turn
            }
            (_, None) => mapped.turn,
        };
        if matches!(entry.body, Body::Interrupted { .. }) {
            mapped.interrupted.insert(turn);
        }
        let changed = changed_by(&entry.body);
        let had = mapped.items.insert(entry.id.clone(), (turn, changed)).map(|(_, c)| c);
        let item = Item { id: ItemId(entry.id.clone()), turn, at_ms: entry.at_ms, body };
        let background = matches!(&item.body, ItemBody::Tool(call)
            if matches!(&call.detail, Some(detail::ToolDetail::Exec(e)) if e.background));
        if background {
            mapped.background.insert(entry.id.clone(), item.clone());
        }
        let open = matches!(&item.body, ItemBody::Tool(call) if !call.state.is_final());
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
    }

    fn remove(&mut self, thread: &conv::ThreadId, entry: &str) {
        let id = self.ensure(thread);
        let Some(mapped) = self.threads.get_mut(thread) else { return };
        let mut actions = vec![Action::ItemRemoved { item: ItemId(entry.to_owned()) }];
        mapped.items.remove(entry);
        mapped.background.remove(entry);
        if let Some(turn) = mapped.prompts.remove(entry) {
            let after = TurnId(turn.0.saturating_sub(1));
            mapped.turn = after;
            mapped.prompts.retain(|_, t| *t < turn);
            mapped.items.retain(|_, (t, _)| *t < turn);
            mapped.turns.retain(|t, _| *t < turn);
            mapped.ended.retain(|t| *t < turn);
            mapped.interrupted.retain(|t| *t < turn);
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
        let state = match record.ended_ms {
            None => TurnState::Active,
            Some(_) if mapped.interrupted.contains(&turn) => TurnState::Interrupted,
            Some(_) => TurnState::Complete,
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
        if let Some(ended_ms) = record.ended_ms
            && mapped.ended.insert(turn)
        {
            actions.push(Action::TurnEnded { turn, state, usage, ended_ms });
        }
        for action in actions {
            self.push(id, action);
        }
    }

    fn output(&mut self, output: &conv::Output) {
        let id = self.ensure(&output.thread);
        let Some(mapped) = self.threads.get_mut(&output.thread) else { return };
        let Some(item) = mapped.background.get_mut(&output.call) else { return };
        if let ItemBody::Tool(call) = &mut item.body {
            call.output = Some(clipped(&output.thread, &output.tail));
        }
        let item = item.clone();
        self.push(id, Action::ItemUpdated(item));
    }

    fn live_start(&mut self, thread: &conv::ThreadId, id: LiveId, kind: &LiveKind) {
        let thread_id = self.ensure(thread);
        let Some(mapped) = self.threads.get(thread) else { return };
        let turn = mapped.turn;
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
            Body::Interrupted { during_tool } => ItemBody::Notice(Notice {
                kind: Notice::INTERRUPTED.to_owned(),
                text: Clipped::whole(if *during_tool {
                    "Interrupted during a tool call"
                } else {
                    "Interrupted"
                }),
            }),
            Body::Note(note) => ItemBody::Notice(Notice {
                kind: match note.kind {
                    NoteKind::ApiError => Notice::API_ERROR,
                    NoteKind::Command => Notice::COMMAND,
                    NoteKind::Info => Notice::INFO,
                    NoteKind::Hook => Notice::HOOK,
                }
                .to_owned(),
                text: clip(&note.text),
            }),
            Body::Rewound { dropped } => ItemBody::Notice(Notice {
                kind: Notice::REWOUND.to_owned(),
                text: Clipped::whole(&format!(
                    "Went back to an earlier message; {dropped} later entries left"
                )),
            }),
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
            conv::ToolDetail::Edit(e) => (
                kind::EDIT,
                format!("Edit {}", e.path),
                Some(detail::ToolDetail::Edit(detail::EditDetail {
                    path: e.path.clone(),
                    edits: e.edits,
                    replace_all: e.replace_all,
                    patch: patch(thread, &e.patch),
                })),
            ),
            conv::ToolDetail::Write(w) => (
                kind::WRITE,
                format!("Write {}", w.path),
                Some(detail::ToolDetail::Write(detail::WriteDetail {
                    path: w.path.clone(),
                    lines: w.lines,
                    created: match w.kind {
                        conv::WriteKind::Unknown => None,
                        conv::WriteKind::Create => Some(true),
                        conv::WriteKind::Overwrite => Some(false),
                    },
                    patch: patch(thread, &w.patch),
                })),
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
                child = a.agent_id.as_ref().map(|agent| subagent_of(self.meta.id, agent));
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

fn patch(thread: &conv::ThreadId, p: &conv::Patch) -> thread::Patch {
    thread::Patch {
        hunks: p
            .hunks
            .iter()
            .map(|h| detail::Hunk {
                old_start: h.old_start,
                old_lines: h.old_lines,
                new_start: h.new_start,
                new_lines: h.new_lines,
                lines: h.lines.clone(),
            })
            .collect(),
        added: p.added,
        removed: p.removed,
        clipped_lines: p.clipped_lines,
        full: p.full.as_ref().map(|r| content_ref(thread, r)),
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

/// What "allow always" grants, in Claude Code's words.
fn grants(prompt: &PermissionPrompt) -> Option<String> {
    let words: Vec<String> = prompt
        .suggestions
        .iter()
        .map(|s| match &s.grant {
            Grant::Rules { rules, .. } => rules.join(", "),
            Grant::Mode { mode } => format!("mode {mode}"),
            Grant::Directories { directories } => directories.join(", "),
            Grant::Other { kind } => kind.clone(),
        })
        .filter(|w| !w.is_empty())
        .collect();
    (!words.is_empty()).then(|| words.join("; "))
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
            (Request::QUESTION, title, vec![deny], question(q).questions, None)
        }
        conv::ToolDetail::Plan { .. } => {
            let allow = choice("allow", "Approve the plan", Effect::Allow, None, false);
            (Request::PLAN, "Approve the plan?".to_owned(), vec![allow, deny], Vec::new(), None)
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
                conv::ToolDetail::Edit(e) => Some(patch(&thread, &e.patch)),
                conv::ToolDetail::Write(w) => Some(patch(&thread, &w.patch)),
                _ => None,
            };
            (Request::APPROVAL, format!("Allow {}?", prompt.tool), options, Vec::new(), proposed)
        }
    };
    let text = match &prompt.detail {
        conv::ToolDetail::Bash(b) => Some(clipped(&thread, &b.command)),
        conv::ToolDetail::Plan { plan } => Some(clipped(&thread, plan)),
        _ => None,
    };
    Request {
        id: AskId(prompt.ask.to_string()),
        item: None,
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
