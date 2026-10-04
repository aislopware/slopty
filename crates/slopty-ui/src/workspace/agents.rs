//! Coding agents in terminals: what the worker says about them, the header badge, the banner
//! when the human is away, and the count of the ones waiting on the human.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason, SessionAgent};
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::SessionSummary;
use slopty_proto::thread::attention::Rung;
use slopty_theme::alpha;

use super::actions::NextAttention;
use super::faces::ThreadWait;
use super::tile::Chrome;
use super::{Finished, WorkspaceEvent, WorkspaceView};
use crate::a11y::tab_stop;
use crate::chrome_text::ChromeText;
use crate::colors::{hsla, hsla_alpha};
use crate::draw::Draw;
use crate::icons::Status;

/// An agent waiting on the human: where, and the tile that shows it, if one does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Waiting {
    /// The worker it runs on.
    pub worker: WorkerKey,
    /// Its tile here.
    pub tile: Option<TileRef>,
    /// Its session.
    pub session: SessionId,
}

/// Something on the attention ladder: an agent in a terminal, or a thread whose row speaks
/// for it, having no terminal whose agent could (Codex, pi, an ACP agent).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Step {
    /// A terminal's agent, or a command that ended in it.
    Session(Waiting),
    /// A thread.
    Thread(ThreadWait),
}

impl Step {
    /// Its tile here, if one shows it.
    pub(super) const fn tile(self) -> Option<TileRef> {
        match self {
            Self::Session(w) => w.tile,
            Self::Thread(w) => w.tile,
        }
    }
}

/// A banner's title: what the agent is doing, led by the tile's name when the human gave it
/// one, so a banner from several agents says which tile it is about.
#[must_use]
pub fn banner_title(name: Option<&str>, what: &str) -> String {
    match name {
        Some(name) => format!("{name} · {what}"),
        None => what.to_owned(),
    }
}

/// A program's notification as a banner: its title led by the tile's name, "Terminal" when
/// the protocol carried no title (OSC 9), and its body.
#[must_use]
pub fn program_banner(name: Option<&str>, title: &str, body: &str) -> (String, String) {
    let title = if title.trim().is_empty() { "Terminal" } else { title.trim() };
    (banner_title(name, title), body.trim().to_owned())
}

/// Whether the agent is waiting on the human (an idle prompt is not worth an outline).
pub(super) fn needs_human(agent: &AgentEvent) -> bool {
    matches!(&agent.status, AgentStatus::Blocked(why) if *why != BlockReason::IdlePrompt)
}

/// What a click on a waiting agent's badge does, as a screen reader says it.
pub(super) const SHOW_PROMPT: &str = "Shows the prompt";

/// One short line for an agent's state: the badge text, the picker's status column.
#[must_use]
pub fn agent_status_text(agent: &AgentEvent) -> String {
    let detail = agent.detail.as_deref().filter(|d| !d.is_empty());
    match &agent.status {
        AgentStatus::None => String::new(),
        AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt) => "Idle".to_owned(),
        AgentStatus::Working => detail.unwrap_or("Working").to_owned(),
        AgentStatus::Tool { tool } => detail.unwrap_or(tool).to_owned(),
        AgentStatus::Blocked(BlockReason::Permission { tool }) => {
            format!("Needs approval: {}", detail.unwrap_or(tool))
        }
        AgentStatus::Blocked(BlockReason::Question) => {
            detail.map_or_else(|| "Has a question".to_owned(), |d| format!("Asks: {d}"))
        }
        AgentStatus::Blocked(BlockReason::Elicitation) => {
            detail.map_or_else(|| "Needs input".to_owned(), |d| format!("Needs input: {d}"))
        }
        AgentStatus::Done => {
            detail.map_or_else(|| "Turn finished".to_owned(), |d| format!("Done: {d}"))
        }
        AgentStatus::Failed { error, until_ms } if error == AgentStatus::RATE_LIMIT => {
            let resets = until_ms
                .and_then(|at| crate::conversation::figures::stamp(at, slopty_core::WallMs::now()));
            resets.map_or_else(
                || "Hit its usage limit".to_owned(),
                |at| format!("Hit its usage limit \u{b7} resets {at}"),
            )
        }
        AgentStatus::Failed { .. } => {
            detail.map_or_else(|| "Turn failed".to_owned(), |d| format!("Failed: {d}"))
        }
        // The turn ended with work still out: the first task's or the loop's own words.
        AgentStatus::Waiting { tasks: 1, .. } if let Some(d) = detail => format!("Waiting on {d}"),
        AgentStatus::Waiting { tasks: 0, .. } => {
            detail.map_or_else(|| "Looping".to_owned(), |d| format!("Looping: {d}"))
        }
        AgentStatus::Waiting { tasks: 1, .. } => "Waiting on a task".to_owned(),
        AgentStatus::Waiting { tasks, .. } => format!("Waiting on {tasks} tasks"),
    }
}

/// The agent's state in a word or two, without the detail: the one word for it wherever a
/// state is said beside something else, a header's pill, a navigator row's trailing word, an
/// *Needs you* row with nothing asked.
///
/// A tool call is "Working": the word names the state, and which tool is the line's to say.
/// "Needs you" is not among them; it heads the section that groups the three waiting words.
#[must_use]
pub(super) fn agent_status_word(agent: &AgentEvent) -> String {
    match &agent.status {
        AgentStatus::None => String::new(),
        AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt) => "Idle".to_owned(),
        AgentStatus::Working | AgentStatus::Tool { .. } => "Working".to_owned(),
        AgentStatus::Blocked(BlockReason::Permission { .. }) => "Needs approval".to_owned(),
        AgentStatus::Blocked(BlockReason::Question) => "Has a question".to_owned(),
        AgentStatus::Blocked(BlockReason::Elicitation) => "Needs input".to_owned(),
        AgentStatus::Done => "Turn finished".to_owned(),
        AgentStatus::Failed { error, .. } if error == AgentStatus::RATE_LIMIT => {
            "Limit reached".to_owned()
        }
        AgentStatus::Failed { .. } => "Failed".to_owned(),
        AgentStatus::Waiting { .. } => "Waiting".to_owned(),
    }
}

/// Whether an agent's state wears a pill in its tile's header: what calls for the person (waiting
/// on them, failed, out of reach), and a turn paused on background work, whose pill says what it
/// waits on. Working, at rest or finished, the header's leading mark says it alone.
const fn wears_pill(status: Status) -> bool {
    matches!(status, Status::NeedsYou | Status::Running | Status::Failed | Status::Away)
}

/// What a waiting agent asks, without the state word: "Bash · touch notes.txt", the question
/// it put, the input it wants. `None` when it waits on nothing, or says nothing more than its
/// state.
///
/// A row whose trailing word already says "Needs you" and a chip that says "Needs approval"
/// take this as their detail, so the state is not said twice in one place. The worker's line
/// leads with the tool's own name or a shell's `$` ("Edit src/main.rs", "$ cargo test"), which
/// the tool before the dot already says.
#[must_use]
pub(super) fn agent_ask_text(agent: &AgentEvent) -> Option<String> {
    let detail = agent.detail.as_deref().map(str::trim).filter(|d| !d.is_empty());
    match &agent.status {
        AgentStatus::Blocked(BlockReason::Permission { tool }) => {
            let subject = detail.map(|d| ask_subject(tool, d)).filter(|s| !s.is_empty());
            match (tool.as_str(), subject) {
                ("", subject) => subject.map(str::to_owned),
                (tool, None) => Some(tool.to_owned()),
                (tool, Some(subject)) => Some(format!("{tool} \u{b7} {subject}")),
            }
        }
        AgentStatus::Blocked(BlockReason::Question | BlockReason::Elicitation) => {
            detail.map(str::to_owned)
        }
        _ => None,
    }
}

/// What a waiting agent asks, as a row can lead with it: the action and its subject ("Run
/// touch notes.txt", [`tool_action`]), or what the tool does when the hook named only the tool
/// ([`tool_statement`]). A row says what is asked; the tool's own name is left to the tooltip
/// ([`agent_ask_text`]).
#[must_use]
pub(super) fn agent_ask_line(agent: &AgentEvent) -> Option<String> {
    let ask = agent_ask_text(agent)?;
    let AgentStatus::Blocked(BlockReason::Permission { tool }) = &agent.status else {
        return Some(ask);
    };
    if tool.is_empty() {
        return Some(ask);
    }
    let subject = agent.detail.as_deref().map(|d| ask_subject(tool, d.trim())).unwrap_or_default();
    Some(if subject.is_empty() { tool_statement(tool) } else { tool_action(tool, subject) })
}

/// Using `tool` on `subject`, said as the action: "Run cargo test", "Edit src/main.rs", "Use
/// query from db: users".
#[must_use]
pub(super) fn tool_action(tool: &str, subject: &str) -> String {
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
pub(super) fn tool_statement(tool: &str) -> String {
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

impl WorkspaceView {
    /// A worker observed a coding agent's state in a session.
    ///
    /// Its detail is the agent's own words, Markdown as the agent writes it, and every place
    /// that says it draws one plain line: it is said as plain words here, once.
    pub fn agent_event(&mut self, mut event: AgentEvent, cx: &mut Context<Self>) {
        event.detail = event.detail.map(|d| crate::markdown::plain_line(&d));
        let session = event.session;
        if let Some(view) = self.terminals.get(&session) {
            let status = (event.status != AgentStatus::None).then(|| event.status.clone());
            view.update(cx, |v, cx| v.set_agent_status(status, cx));
        }
        if let Some(face) = self.faces.views.get(&session) {
            let agent = (event.status != AgentStatus::None).then(|| event.clone());
            face.update(cx, |v, cx| v.set_agent(agent, cx));
        }
        if let Some(elapsed) = self.agent_turn(&event) {
            self.agent_finished(&event, elapsed, cx);
        }
        if event.status == AgentStatus::None {
            self.agents.remove(&session);
        } else {
            let attention = event.attention;
            let asks = Status::of_agent(&event) == Some(Status::NeedsYou)
                && self.agents.get(&session).and_then(Status::of_agent) != Some(Status::NeedsYou);
            let word = asks.then(|| agent_status_word(&event).to_lowercase());
            self.agents.insert(session, event);
            if attention {
                cx.emit(WorkspaceEvent::Attention(session));
            }
            if let (Some(word), Some(tile)) = (word, self.tile_of_session(session)) {
                self.attention_toast(tile, Status::NeedsYou, &word, cx);
            }
        }
        self.update_awake(cx);
        self.agents_moved(cx);
        self.update_run_targets(cx);
        cx.notify();
    }

    /// What the worker's summaries say runs in each session, taken before any `Agent` event
    /// arrives, so a tile shows its agent's badge, and offers the hooks only when they are not
    /// what the worker reads it from, from the first frame after a connect. A session that
    /// already has a live event keeps it: the event is newer than any summary.
    pub(super) fn seed_agents<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a SessionSummary>,
        cx: &mut Context<Self>,
    ) {
        let mut seeded = false;
        for summary in sessions {
            let Some(SessionAgent { kind, status, source, since_ms, mode }) = summary.agent.clone()
            else {
                continue;
            };
            if status == AgentStatus::None || self.agents.contains_key(&summary.id) {
                continue;
            }
            if let Some(view) = self.terminals.get(&summary.id) {
                let status = status.clone();
                view.update(cx, |v, cx| v.set_agent_status(Some(status), cx));
            }
            self.agents.insert(
                summary.id,
                AgentEvent {
                    session: summary.id,
                    kind,
                    status,
                    agent_session: None,
                    detail: None,
                    attention: false,
                    source,
                    since_ms,
                    mode,
                },
            );
            seeded = true;
        }
        if seeded {
            self.update_awake(cx);
            self.agents_moved(cx);
            self.update_run_targets(cx);
        }
    }

    /// Hold the device awake while any agent works; let go when none does.
    pub(super) fn update_awake(&mut self, cx: &Context<Self>) {
        let working = self
            .agents
            .values()
            .any(|a| matches!(a.status, AgentStatus::Working | AgentStatus::Tool { .. }));
        if !working {
            self.awake = None;
        } else if self.awake.is_none() {
            let acquisition = cx.prevent_idle_sleep("Slopty agent working");
            self.awake = Some(cx.spawn(async move |_this, _cx| match acquisition.await {
                Ok(guard) => {
                    let _guard = guard;
                    std::future::pending::<()>().await;
                }
                Err(e) => tracing::warn!(error = %e, "idle sleep prevention"),
            }));
        }
    }

    /// Sessions whose agent is waiting on the human: those with a tile in reading order
    /// (workspace, column, tile) so ⌘⇧A walks the strip predictably, then those the server
    /// reported on a worker with no tile for them here.
    pub(super) fn needs_you(&self) -> Vec<Waiting> {
        let mut shown: Vec<(Option<slopty_client::layout::Pos>, Waiting)> = self
            .items()
            .filter_map(|(worker, i)| match i.kind {
                ItemKind::Terminal { session } => {
                    Some(Waiting { worker, tile: Some(TileRef { worker, item: i.id }), session })
                }
                _ => None,
            })
            .filter(|w| self.agent_state(w.session).is_some_and(needs_human))
            .map(|w| (w.tile.and_then(|t| self.layout.position(t)), w))
            .collect();
        shown.sort_by_key(|(pos, w)| {
            (pos.map(|p| (p.workspace, p.column, p.tile)), w.tile.map(|t| t.item))
        });
        let mut unshown: Vec<Waiting> = self
            .server_agents
            .iter()
            .filter(|(session, (_, agent))| {
                needs_human(agent) && self.tile_of_session(**session).is_none()
            })
            .map(|(session, (worker, _))| Waiting {
                worker: *worker,
                tile: None,
                session: *session,
            })
            .collect();
        unshown.sort_by_key(|w| (w.worker, w.session));
        shown.into_iter().map(|(_, w)| w).chain(unshown).collect()
    }

    /// The agents whose turn ended while nobody looked, left to review: unread,
    /// in reading order.
    pub(super) fn to_review(&self) -> Vec<Waiting> {
        let mut ended: Vec<(Option<slopty_client::layout::Pos>, Waiting)> = self
            .agent_turns()
            .filter_map(|session| {
                let tile = self.tile_of_session(session)?;
                let at = Waiting { worker: tile.worker, tile: Some(tile), session };
                Some((self.layout.position(tile), at))
            })
            .collect();
        ended.sort_by_key(|(pos, w)| (pos.map(|p| (p.workspace, p.column, p.tile)), w.session));
        ended.into_iter().map(|(_, w)| w).collect()
    }

    /// What is known about a session's agent: the worker's own word while its link is up,
    /// else the server's.
    pub(super) fn agent_state(&self, session: SessionId) -> Option<&AgentEvent> {
        self.agents.get(&session).or_else(|| self.server_agents.get(&session).map(|(_, a)| a))
    }

    /// The server relayed an agent's change on `worker`. It counts toward the agents that need
    /// the human whether or not the worker's own link is up or a tile shows the session, and
    /// raises the banner when no link of this client's would.
    pub fn server_agent_event(
        &mut self,
        worker: WorkerKey,
        event: AgentEvent,
        cx: &mut Context<Self>,
    ) {
        let session = event.session;
        let linked = self.workers.get(&worker).is_some_and(|w| w.link.is_some());
        if event.status == AgentStatus::None {
            self.server_agents.remove(&session);
        } else {
            if event.attention && !linked {
                cx.emit(WorkspaceEvent::Attention(session));
            }
            self.server_agents.insert(session, (worker, event));
        }
        self.agents_moved(cx);
        cx.notify();
    }

    /// Every agent the server knows, as its state after a connect or a lag gives them: what it
    /// said before is replaced. None raises attention; a change after it does.
    pub fn server_agents_replace(
        &mut self,
        agents: Vec<(WorkerKey, AgentEvent)>,
        cx: &mut Context<Self>,
    ) {
        self.server_agents = agents
            .into_iter()
            .filter(|(_, event)| event.status != AgentStatus::None)
            .map(|(worker, event)| (event.session, (worker, event)))
            .collect();
        self.agents_moved(cx);
        cx.notify();
    }

    /// The server says a session ended.
    pub fn server_session_closed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.server_agents.remove(&session).is_some() {
            self.agents_moved(cx);
            cx.notify();
        }
    }

    /// Drop what the server said about agents and their threads: on `worker` (gone), or
    /// everywhere (`None`, the server was disconnected).
    pub fn forget_server_agents(&mut self, worker: Option<WorkerKey>, cx: &mut Context<Self>) {
        let before = self.server_agents.len();
        self.server_agents.retain(|_, (w, _)| worker.is_some_and(|gone| *w != gone));
        let threads = self.forget_server_threads(worker);
        if self.server_agents.len() != before || threads {
            self.agents_moved(cx);
            cx.notify();
        }
    }

    /// How many agents are waiting on the human, on every worker.
    #[must_use]
    pub fn needs_you_count(&self) -> usize {
        self.needs_you().len().saturating_add(self.threads_waiting().len())
    }

    /// Agents waiting on the human on one worker.
    #[must_use]
    pub fn needs_you_on(&self, worker: WorkerKey) -> usize {
        let sessions = self.needs_you().iter().filter(|w| w.worker == worker).count();
        let threads = self.threads_waiting().iter().filter(|w| w.worker == worker).count();
        sessions.saturating_add(threads)
    }

    /// What some agent is doing changed: tell the app the count (the Dock badge), hand the
    /// boards on show their agents' word in the next frame, and take down the corner's word
    /// about any that was answered.
    pub(super) fn agents_moved(&mut self, cx: &mut Context<Self>) {
        self.projects.dirty = true;
        if self.drop_answered_attention() {
            cx.notify();
        }
        cx.emit(WorkspaceEvent::NeedsYou(self.needs_you_count()));
    }

    /// What wants the person, on every worker, in the ladder's order: the agents and threads
    /// that need them, then what failed (the finishes not yet looked at and the threads that
    /// stopped on an error), then the rest of those finishes. Each rung in reading order, what
    /// has no tile here after it.
    pub(super) fn attention_ladder(&self) -> Vec<Step> {
        let finished = |failed: bool| {
            self.finished
                .iter()
                .filter(|(_, done)| done.exit.is_some_and(|e| e != 0) == failed)
                .filter_map(|(session, _)| {
                    let tile = self.tile_of_session(*session)?;
                    let at = Waiting { worker: tile.worker, tile: Some(tile), session: *session };
                    Some(Step::Session(at))
                })
                .collect::<Vec<_>>()
        };
        let threads = |rung: Rung| self.threads_on(rung).into_iter().map(Step::Thread);
        let mut ladder = self.steps_in_reading_order(
            self.needs_you().into_iter().map(Step::Session).chain(threads(Rung::NeedsYou)),
        );
        ladder.extend(
            self.steps_in_reading_order(finished(true).into_iter().chain(threads(Rung::Failed))),
        );
        ladder.extend(self.steps_in_reading_order(finished(false)));
        ladder
    }

    /// `steps` in reading order (workspace, column, tile), those with no tile here after them
    /// in the order they came.
    pub(super) fn steps_in_reading_order(
        &self,
        steps: impl IntoIterator<Item = Step>,
    ) -> Vec<Step> {
        let mut steps: Vec<(Option<slopty_client::layout::Pos>, Step)> = steps
            .into_iter()
            .map(|step| (step.tile().and_then(|t| self.layout.position(t)), step))
            .collect();
        steps.sort_by_key(|(pos, _)| (pos.is_none(), pos.map(|p| (p.workspace, p.column, p.tile))));
        steps.into_iter().map(|(_, step)| step).collect()
    }

    /// ⌘⇧A: reveal and focus the next thing on the attention ladder, on whichever worker:
    /// the agents that need the person first, then what failed, then what finished unseen,
    /// cycling from the focused tile.
    pub fn next_attention(
        &mut self,
        _: &NextAttention,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ladder = self.attention_ladder();
        if ladder.is_empty() {
            return;
        }
        // From the focused rung, the next; from a finish the last step looked at (and so took
        // off the ladder), the one that took its place.
        let at = match self.focused().and_then(|f| ladder.iter().position(|w| w.tile() == Some(f)))
        {
            Some(i) => i.saturating_add(1),
            None => self.attention_at.unwrap_or(0),
        };
        let at = if at < ladder.len() { at } else { 0 };
        self.attention_at = Some(at);
        let Some(next) = ladder.get(at).copied() else { return };
        self.reveal_step(next, cx);
    }

    /// Bring up what `step` is about: a terminal's tile, a thread's, or, with none here, a
    /// new one on its worker. A worker this client cannot reach says so instead.
    pub(super) fn reveal_step(&mut self, step: Step, cx: &mut Context<Self>) {
        match step {
            Step::Session(w) if w.tile.is_some() => self.reveal_session(w.session, cx),
            Step::Session(w) => self.show_untiled(w.worker, w.session, cx),
            Step::Thread(ThreadWait { tile: Some(tile), .. }) => self.go_to(tile.item, cx),
            Step::Thread(w) => {
                let Some(worker) = self.workers.get(&w.worker) else { return };
                if worker.link.is_none() {
                    let text = format!("{} is not reachable from here", worker.name);
                    self.show_notice(text, cx);
                    return;
                }
                self.open_thread(w.worker, w.thread, cx);
            }
        }
    }

    /// An agent that needs the human in a session with no tile here: give it one, which the
    /// worker then syncs to every client. A worker this client cannot reach says so instead.
    pub(super) fn show_untiled(
        &mut self,
        worker: WorkerKey,
        session: SessionId,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get(&worker) else { return };
        if w.link.is_none() {
            let text = format!("{} is not reachable from here", w.name);
            self.show_notice(text, cx);
            return;
        }
        let item = slopty_proto::items::Item {
            id: slopty_core::ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: std::collections::BTreeMap::new(),
        };
        self.propose(worker, slopty_proto::items::ItemOp::Add(item), cx);
        self.pending_focus = Some(session);
    }

    /// A shell command ended in `session`. Long enough, and not watched (on a tile other than
    /// the focused one, or with the app away), it earns its tile's unseen dot, cleared when the
    /// tile is focused. Neither the corner nor the bell says it: they speak for agents.
    pub fn command_finished(&mut self, session: SessionId, done: Finished, cx: &mut Context<Self>) {
        let tile = self.tile_of_session(session);
        let watched = self.app_active && tile.is_some_and(|t| self.focused() == Some(t));
        let slow = done.elapsed >= self.slow_command;
        tracing::info!(%session, watched, slow, elapsed = ?done.elapsed, "command finished");
        if watched || !slow {
            return;
        }
        self.turns.command_ended(session);
        self.finished.insert(session, done);
        cx.notify();
    }

    /// Ask `worker` to register `slopty hook` for its Claude Code. Offered once per run.
    pub fn install_hooks(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        if let Some(w) = self.workers.get_mut(&worker) {
            w.hooks_offered = true;
            w.send(ClientMsg::InstallHooks);
        }
        cx.notify();
    }

    /// The worker could not install the hooks: put the offer back so it can be tried again.
    pub fn hooks_offer_failed(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        if let Some(w) = self.workers.get_mut(&worker) {
            w.hooks_offered = false;
        }
        cx.notify();
    }

    /// The agent pill in a terminal's header: a short line in the tone of its status, which the
    /// status mark beside it draws as an icon. While the agent waits on the human the pill is a
    /// button that brings the terminal up so the TUI's own prompt can be answered there. Slopty
    /// never answers for the human. On a phone, which has no room for the detail, and beside a
    /// face, which shows it itself, the pill says the state alone; a screen reader still hears
    /// all of it. An idle or a working agent has no pill ([`wears_pill`]): the slot's mark,
    /// spinning while it works, says all there is, and a grey "Working" beside the spinner said it
    /// twice.
    pub(super) fn agent_badge(
        &self,
        tile: TileRef,
        session: SessionId,
        agent: &AgentEvent,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let k = chrome.k;
        // The pill's tone is its status mark's. Working, at rest or finished, the slot's mark
        // is the statement.
        let status = Status::of_agent(agent).filter(|s| wears_pill(*s))?;
        let (full, color) = (agent_status_text(agent), status.tone(theme));
        // The word alone: what is asked is the navigator's line and the pointer's, not a
        // second sentence in every header (a screen reader still hears it in full).
        let label = agent_status_word(agent);
        let ask = agent_ask_text(agent);
        let item = tile.item;
        // An agent waiting on the human is the one state worth a click: the badge itself
        // goes to it. No second "go" beside it — a click on the tile did the same.
        let waiting = matches!(
            agent.status,
            AgentStatus::Blocked(
                BlockReason::Permission { .. } | BlockReason::Question | BlockReason::Elicitation,
            )
        );
        let ui_size = theme.typography.small() * k;
        let pill = crate::kit::pill(theme, color, k)
            .id("agent")
            .debug_selector(move || format!("agent-{}", item.as_uuid()))
            .role(if waiting { Role::Button } else { Role::Status })
            .aria_label(SharedString::from(full))
            // The answer belongs to the agent's own prompt; the click only goes there.
            .when(waiting, |el| el.aria_description(SHOW_PROMPT))
            .when_some(ask, |el, ask| {
                let theme = std::rc::Rc::new(theme.clone());
                crate::kit::hint_timing(el).tooltip(move |_window, cx| {
                    let (ask, theme) = (ask.clone(), std::rc::Rc::clone(&theme));
                    cx.new(|_| crate::kit::Hint::new(ask, "", theme)).into()
                })
            })
            .min_w_0()
            .max_w(px(ui_size * 22.0))
            .child(
                div().min_w_0().overflow_hidden().child(
                    ChromeText::new(label, px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                ),
            );
        let pill = if waiting {
            tab_stop(
                pill.cursor_pointer().hover(move |el| el.bg(hsla_alpha(color, alpha::TINT))),
                theme.surfaces.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.reveal_session(session, cx)))
        } else {
            pill
        };
        Some(
            div()
                .id("badge")
                .debug_selector(move || format!("badge-{}", item.as_uuid()))
                .flex()
                .min_w_0()
                .items_center()
                .child(pill)
                .into_any_element(),
        )
    }

    /// The pill in a thread tile's header: where its thread stands, in the word and the tone a
    /// terminal agent's pill would have ("Needs approval", "Has a question"), what it asks in its
    /// hint. A thread driven over a protocol has no terminal to say it; working, at rest, or once
    /// done, it wears none, as a terminal agent's tile does.
    pub(super) fn thread_badge(
        &self,
        tile: TileRef,
        stand: &super::faces::ThreadStand,
        chrome: Chrome,
    ) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let k = chrome.k;
        let status = stand.status().filter(|s| wears_pill(*s))?;
        let label = stand.word()?;
        let item = tile.item;
        let ask = stand
            .asks
            .as_ref()
            .map(|a| crate::markdown::plain_line(&a.title))
            .filter(|t| !t.trim().is_empty());
        let full = ask.as_ref().map_or_else(|| label.to_owned(), |ask| format!("{label}: {ask}"));
        let ui_size = theme.typography.small() * k;
        let pill = crate::kit::pill(theme, status.tone(theme), k)
            .id("agent")
            .debug_selector(move || format!("agent-{}", item.as_uuid()))
            .role(Role::Status)
            .aria_label(SharedString::from(full))
            .when_some(ask, |el, ask| {
                let theme = std::rc::Rc::new(theme.clone());
                crate::kit::hint_timing(el).tooltip(move |_window, cx| {
                    let (ask, theme) = (ask.clone(), std::rc::Rc::clone(&theme));
                    cx.new(|_| crate::kit::Hint::new(ask, "", theme)).into()
                })
            })
            .min_w_0()
            .max_w(px(ui_size * 22.0))
            .child(
                div().min_w_0().overflow_hidden().child(
                    ChromeText::new(label, px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                ),
            );
        Some(
            div()
                .id("badge")
                .debug_selector(move || format!("badge-{}", item.as_uuid()))
                .flex()
                .min_w_0()
                .items_center()
                .child(pill)
                .into_any_element(),
        )
    }

    /// The badge for a long shell command that ended unwatched: its status and how long it
    /// took, in the tone of its status mark (done or failed). A press focuses the tile (which
    /// clears it).
    pub(super) fn finished_badge(
        &self,
        tile: TileRef,
        session: SessionId,
        done: &Finished,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        // The status slot beside it already says done or failed in its tone, and the unseen
        // dot that it went unwatched: this is only the readout, quiet, still a button to it.
        let quiet = theme.surfaces.text_secondary;
        let label = done.label();
        let item = tile.item;
        let pill = crate::kit::pill_frame(theme, k)
            .id("finished")
            .debug_selector(move || format!("finished-{}", item.as_uuid()))
            .role(Role::Button)
            .aria_label(label.clone())
            .flex_none()
            .text_color(hsla(quiet))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla_alpha(quiet, alpha::FAINT)))
            .child(
                ChromeText::new(label, px(theme.typography.small()), k)
                    .fill()
                    .zooming(chrome.zooming),
            );
        tab_stop(pill, theme.surfaces.accent)
            .on_click(cx.listener(move |this, _ev, _window, cx| this.reveal_session(session, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::agent::{AgentKind, AgentSource};

    use super::*;

    fn asking(status: AgentStatus, detail: Option<&str>) -> AgentEvent {
        AgentEvent {
            session: SessionId::new(),
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: detail.map(str::to_owned),
            attention: false,
            source: AgentSource::Hook,
            since_ms: WallMs::ZERO,
            mode: None,
        }
    }

    fn permission(tool: &str, detail: Option<&str>) -> Option<String> {
        let tool = tool.to_owned();
        agent_ask_text(&asking(AgentStatus::Blocked(BlockReason::Permission { tool }), detail))
    }

    /// The ask is the tool and its subject, with no state word and no `$`, and a tool's own
    /// name is not said twice. A state with nothing asked has no ask.
    #[test]
    fn the_ask_says_what_is_asked_without_the_state() {
        let cases = [
            ("Bash", Some("$ touch refused.txt"), Some("Bash \u{b7} touch refused.txt")),
            ("Edit", Some("Edit src/main.rs"), Some("Edit \u{b7} src/main.rs")),
            ("WebFetch", Some("Fetch https://a.b"), Some("WebFetch \u{b7} https://a.b")),
            ("Task", Some("Agent: count lines"), Some("Task \u{b7} count lines")),
            ("Skill", Some("/commit"), Some("Skill \u{b7} /commit")),
            ("Bash", None, Some("Bash")),
            ("mcp__db__query", Some("mcp__db__query"), Some("mcp__db__query")),
            ("Bash", Some("Editing notes"), Some("Bash \u{b7} Editing notes")),
            ("", Some("$ ls"), Some("ls")),
            ("", None, None),
        ];
        for (tool, detail, ask) in cases {
            assert_eq!(permission(tool, detail).as_deref(), ask, "{tool} {detail:?}");
        }
        let question = AgentStatus::Blocked(BlockReason::Question);
        let asked = asking(question.clone(), Some("Which branch?"));
        assert_eq!(agent_ask_text(&asked).as_deref(), Some("Which branch?"));
        assert_eq!(agent_ask_text(&asking(question, None)), None);
        let working = asking(AgentStatus::Working, Some("Editing src/main.rs"));
        assert_eq!(agent_ask_text(&working), None, "working asks nothing");
    }

    /// A turn paused on work in the background says what it waits on: the one task's words, a
    /// count when there are more or no words, and a loop's own prompt. It is busy, calmly: not
    /// at rest, and nothing asked.
    #[test]
    fn a_paused_turn_says_what_it_waits_on() {
        let waiting = |tasks, crons, detail| asking(AgentStatus::Waiting { tasks, crons }, detail);
        let cases = [
            (waiting(1, 0, Some("npm test")), "Waiting on npm test"),
            (waiting(2, 0, Some("npm test")), "Waiting on 2 tasks"),
            (waiting(1, 0, None), "Waiting on a task"),
            (waiting(3, 1, None), "Waiting on 3 tasks"),
            (waiting(0, 1, Some("check the deploy")), "Looping: check the deploy"),
            (waiting(0, 2, None), "Looping"),
        ];
        for (agent, text) in cases {
            assert_eq!(agent_status_text(&agent), text);
            assert_eq!(agent_status_word(&agent), "Waiting");
            assert_eq!(Status::of_agent(&agent), Some(Status::Running));
            assert!(!needs_human(&agent) && agent_ask_text(&agent).is_none());
        }
    }

    /// A turn that hit a usage limit says so, and when the limit resets where that is known;
    /// another failure says what failed. The word stays short either way.
    #[test]
    fn a_failed_turn_says_why_and_when_a_limit_resets() {
        let failed = |error: &str, until_ms| {
            asking(AgentStatus::Failed { error: error.to_owned(), until_ms }, None)
        };
        let limited = failed(AgentStatus::RATE_LIMIT, None);
        assert_eq!(agent_status_text(&limited), "Hit its usage limit");
        assert_eq!(agent_status_word(&limited), "Limit reached");
        let at = WallMs::now();
        let clock = crate::conversation::figures::stamp(at, at).unwrap_or_default();
        let resets = failed(AgentStatus::RATE_LIMIT, Some(at));
        assert_eq!(
            agent_status_text(&resets),
            format!("Hit its usage limit \u{b7} resets {clock}")
        );
        let other = failed("overloaded", None);
        assert_eq!(agent_status_text(&other), "Turn failed");
        assert_eq!(agent_status_word(&other), "Failed");
    }

    /// The word for a state never carries the detail, so a chip, a pill and a row read one
    /// state the same whatever the agent is doing.
    #[test]
    fn a_state_has_one_word_whatever_its_detail() {
        let tool = "Bash".to_owned();
        let states = [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Tool { tool: "Edit".to_owned() },
            AgentStatus::Blocked(BlockReason::Permission { tool }),
            AgentStatus::Blocked(BlockReason::Question),
            AgentStatus::Blocked(BlockReason::Elicitation),
            AgentStatus::Done,
        ];
        for status in states {
            let bare = agent_status_word(&asking(status.clone(), None));
            let detailed = agent_status_word(&asking(status.clone(), Some("$ touch x")));
            assert_eq!(bare, detailed, "{status:?}");
            assert!(!bare.contains(':'), "{status:?}: {bare}");
        }
        let tool = asking(AgentStatus::Tool { tool: "Bash".to_owned() }, None);
        assert_eq!(agent_status_word(&tool), "Working", "a tool call is the working state");
    }

    /// A row leads with what is asked as an action on its subject; when the hook named only the
    /// tool, with what the tool does. Never the bare name.
    #[test]
    fn a_bare_tool_is_said_as_what_it_asks() {
        let ask = |tool: &str, detail| {
            let tool = tool.to_owned();
            agent_ask_line(&asking(AgentStatus::Blocked(BlockReason::Permission { tool }), detail))
        };
        assert_eq!(ask("Bash", None).as_deref(), Some("Wants to run a command"));
        assert_eq!(ask("mcp__db__query", None).as_deref(), Some("Wants to use query from db"));
        assert_eq!(ask("Frobnicate", None).as_deref(), Some("Wants to use Frobnicate"));
        let detailed = [
            ("Bash", "$ touch x", "Run touch x"),
            ("Edit", "Edit src/main.rs", "Edit src/main.rs"),
            ("WebFetch", "Fetch https://a.b", "Fetch https://a.b"),
            ("Task", "Agent: count lines", "Start a subagent: count lines"),
            ("Skill", "/commit", "Use /commit"),
            ("mcp__db__query", "users", "Use query from db: users"),
        ];
        for (tool, detail, line) in detailed {
            assert_eq!(
                ask(tool, Some(detail)).as_deref(),
                Some(line),
                "the action and its subject"
            );
        }
        let question = asking(AgentStatus::Blocked(BlockReason::Question), Some("Which?"));
        assert_eq!(agent_ask_line(&question).as_deref(), Some("Which?"));
    }
}
