//! Coding agents in terminals and threads: what their rows in the workers' thread tables say of
//! them, a finished command's badge, the banner when the human is away, and the count of the ones
//! waiting on the human.

use gpui::accesskit::Role;
use gpui::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, px,
};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::SessionId;
use slopty_proto::items::ItemKind;
use slopty_proto::thread::attention::Rung;
use slopty_proto::thread::{Phase, Request, Wait};
use slopty_theme::alpha;

use super::actions::NextAttention;
use super::attention::About;
use super::faces::{ThreadStand, ThreadWait};
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

/// What `step` is about, to tell the same agent reached two ways.
const fn step_about(step: Step) -> About {
    match step {
        Step::Session(w) => About::Session(w.session),
        Step::Thread(w) => About::Thread(w.thread),
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

/// What a click on a waiting agent's badge does, as a screen reader says it.
pub(super) const SHOW_PROMPT: &str = "Shows the prompt";

/// Whether the agent is waiting on the human.
pub(super) fn needs_human(agent: &ThreadStand) -> bool {
    agent.rung == Rung::NeedsYou
}

/// The agent's mark in the one vocabulary: its rung, and at rest whether its turn ended
/// (done) or it never had one (idle).
#[must_use]
pub(super) fn agent_mark_of(agent: &ThreadStand) -> Status {
    match agent.rung {
        Rung::NeedsYou => Status::NeedsYou,
        Rung::Failed => Status::Failed,
        Rung::Working => Status::Working,
        Rung::Waiting => Status::Running,
        Rung::ToReview => Status::Done,
        Rung::Idle if agent.status.as_ref().is_some_and(|s| s.phase == Phase::Done) => Status::Done,
        Rung::Idle => Status::Idle,
    }
}

/// What its row says it waits on, in words, when it says any.
fn wait_text(agent: &ThreadStand) -> Option<&str> {
    let wait = agent.status.as_ref()?.wait.as_ref()?;
    Some(wait.text.trim()).filter(|t| !t.is_empty())
}

/// Its wait's kind, as its row names it.
fn wait_kind(agent: &ThreadStand) -> Option<&str> {
    Some(agent.status.as_ref()?.wait.as_ref()?.kind.as_str())
}

/// What kind of thing a waiting agent asks for: its open request's kind, else its wait's
/// (`permission` is an approval, `input` an elicitation).
fn asked_kind(agent: &ThreadStand) -> Option<&str> {
    agent.asks.as_ref().map(|a| a.kind.as_str()).or_else(|| match wait_kind(agent)? {
        "permission" => Some(Request::APPROVAL),
        "question" => Some(Request::QUESTION),
        "input" => Some(Request::ELICITATION),
        _ => None,
    })
}

/// Whether it stopped on its plan's usage limit.
fn limited(agent: &ThreadStand) -> bool {
    agent.rung == Rung::Failed && wait_kind(agent) == Some(Wait::LIMIT)
}

/// One short line for an agent's state: the badge text, the picker's status column.
#[must_use]
pub(super) fn agent_status_text(agent: &ThreadStand) -> String {
    let word = agent_status_word(agent);
    match agent_mark_of(agent) {
        Status::NeedsYou => {
            // An ask that says no more than the word (an adapter's "Has a question") adds
            // nothing to it.
            let ask = agent_ask_text(agent).filter(|ask| *ask != word);
            match (asked_kind(agent), ask) {
                (Some(Request::QUESTION), Some(ask)) => format!("Asks: {ask}"),
                (_, Some(ask)) => format!("{word}: {ask}"),
                (_, None) => word,
            }
        }
        Status::Working => agent.doing.as_deref().map_or(word, crate::markdown::plain_line),
        // At rest with work out: the commands it left running, as its thread's header says
        // them, or the work it waits on.
        Status::Running => match (wait_kind(agent), wait_text(agent)) {
            (Some(Wait::COMMAND), Some(text)) => format!("Running {text}"),
            (_, Some(text)) => format!("Waiting on {text}"),
            (_, None) => word,
        },
        Status::Failed if limited(agent) => {
            let resets = agent
                .resets
                .and_then(|at| crate::conversation::figures::stamp(at, slopty_core::WallMs::now()));
            let said = wait_text(agent).unwrap_or(LIMIT_TEXT);
            resets.map_or_else(|| said.to_owned(), |at| format!("{said} \u{b7} resets {at}"))
        }
        Status::Failed => "Turn failed".to_owned(),
        _ => word,
    }
}

/// What an agent stopped on its plan's usage limit says, where its row words it not.
const LIMIT_TEXT: &str = "Hit its usage limit";

/// The agent's state in a word or two, without the detail: the one word for it wherever a
/// state is said beside something else, a header's pill, a navigator row's trailing word, an
/// *Needs you* row with nothing asked.
///
/// A tool call is "Working": the word names the state, and which call is the line's to say.
#[must_use]
pub(super) fn agent_status_word(agent: &ThreadStand) -> String {
    match agent_mark_of(agent) {
        Status::NeedsYou => match asked_kind(agent) {
            Some(Request::APPROVAL) => "Needs approval",
            Some(Request::QUESTION) => "Has a question",
            Some(Request::ELICITATION) => "Needs input",
            Some(Request::PLAN) => "Plan to approve",
            _ => "Needs you",
        },
        Status::Working => "Working",
        Status::Running => "Waiting",
        Status::Done => "Turn finished",
        Status::Failed if limited(agent) => "Limit reached",
        Status::Failed => "Failed",
        _ => "Idle",
    }
    .to_owned()
}

/// What a waiting agent asks, without the state word: the call it wants to make ("$ cargo
/// test"), the question it put, the input it wants. `None` when it waits on nothing, or says
/// nothing more than its state.
///
/// An approval is said as its adapter words what it waits on ("Run cargo test"), else by the
/// call that waits on it; a question by its own words. A row whose trailing word already says
/// "Needs you" and a chip that says "Needs approval" take this as their detail, so the state is
/// not said twice in one place.
#[must_use]
pub(super) fn agent_ask_text(agent: &ThreadStand) -> Option<String> {
    if !needs_human(agent) {
        return None;
    }
    let card = agent.asks.as_ref().map(|a| a.title.trim()).filter(|t| !t.is_empty());
    let call = agent.doing.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let said = match asked_kind(agent) {
        Some(Request::APPROVAL) => wait_text(agent).or(call).or(card),
        _ => card.or_else(|| wait_text(agent)),
    };
    said.map(crate::markdown::plain_line)
}

impl WorkspaceView {
    /// Hold the device awake while any agent works; let go when none does.
    pub(super) fn update_awake(&mut self, cx: &Context<Self>) {
        let working = self.any_working();
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

    /// Sessions whose agent is waiting on the human, or whose program is (`OSC 7501`) where no
    /// agent speaks for the tile: those with a tile in reading order
    /// (project, tab, pane) so ⌘⇧A walks the tiles predictably, then those the server's
    /// ladder names on a worker with no tile for them here.
    pub(super) fn needs_you(&self) -> Vec<Waiting> {
        let mut shown: Vec<(Option<usize>, Waiting)> = self
            .items()
            .filter_map(|(worker, i)| match i.kind {
                ItemKind::Terminal { session } => {
                    Some(Waiting { worker, tile: Some(TileRef { worker, item: i.id }), session })
                }
                _ => None,
            })
            .filter(|w| {
                self.agent_state(w.session).is_some_and(needs_human) || self.program_asks(w.session)
            })
            .map(|w| (w.tile.and_then(|t| self.reading_rank(t)), w))
            .collect();
        shown.sort_by_key(|(rank, w)| (*rank, w.tile.map(|t| t.item)));
        let mut unshown: Vec<Waiting> = self
            .agent_sessions()
            .filter(|(session, stand)| {
                needs_human(stand) && self.tile_of_session(*session).is_none()
            })
            .map(|(session, stand)| Waiting { worker: stand.worker, tile: None, session })
            .collect();
        unshown.sort_by_key(|w| (w.worker, w.session));
        shown.into_iter().map(|(_, w)| w).chain(unshown).collect()
    }

    /// What is left for the person to review, in reading order, each once: the agents whose
    /// worker says their changes wait unkept (their row's [`Rung::ToReview`], the same on every
    /// device, until the person keeps them), then the turns that ended while nobody looked. A
    /// terminal's agent counts while its tile is here; a thread with no terminal (Codex beside
    /// no TUI, pi, an ACP agent, a message's runs) counts with or without one.
    pub(super) fn to_review(&self) -> Vec<Step> {
        let waiting = self
            .agent_sessions()
            .filter(|(_, stand)| stand.rung == Rung::ToReview)
            .filter_map(|(session, _)| self.step_of(About::Session(session)))
            .chain(self.threads_on(Rung::ToReview).into_iter().map(Step::Thread));
        let unread = self.agent_turns().filter_map(|about| self.step_of(about));
        let mut steps: Vec<Step> = Vec::new();
        for step in self.steps_in_reading_order(waiting).into_iter().chain(unread) {
            let about = step_about(step);
            if !steps.iter().any(|s| step_about(*s) == about) {
                steps.push(step);
            }
        }
        steps
    }

    /// The ladder's step for what `about` names: a terminal whose tile is here, or a thread
    /// its worker's table holds, with its tile when one shows it.
    pub(super) fn step_of(&self, about: About) -> Option<Step> {
        match about {
            About::Session(session) => {
                let tile = self.tile_of_session(session)?;
                Some(Step::Session(Waiting { worker: tile.worker, tile: Some(tile), session }))
            }
            About::Thread(thread) => {
                let worker = self.thread_stand(thread)?.worker;
                let tile = self.tile_of_thread(thread);
                Some(Step::Thread(ThreadWait { worker, thread, tile }))
            }
        }
    }

    /// Drop what the server said about threads: on `worker` (gone), or everywhere (`None`,
    /// the server was disconnected).
    pub fn forget_server_agents(&mut self, worker: Option<WorkerKey>, cx: &mut Context<Self>) {
        if self.forget_server_threads(worker) {
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
                .filter_map(|(about, _)| self.step_of(*about))
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
        let mut steps: Vec<(Option<usize>, Step)> = steps
            .into_iter()
            .map(|step| (step.tile().and_then(|t| self.reading_rank(t)), step))
            .collect();
        steps.sort_by_key(|(rank, _)| (rank.is_none(), *rank));
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
        self.finished.insert(About::Session(session), done);
        cx.notify();
    }

    /// The badge for a long shell command that ended unwatched: its status and how long it
    /// took, in the tone of its status mark (done or failed). A press focuses the tile (which
    /// clears it).
    pub(super) fn finished_badge(
        &self,
        tile: TileRef,
        session: SessionId,
        done: &Finished,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        // The status slot beside it already says done or failed in its tone, and the unseen
        // dot that it went unwatched: this is only the readout, quiet, still a button to it.
        let quiet = theme.surfaces.text_secondary;
        let label = done.label();
        let item = tile.item;
        let pill = crate::kit::pill_frame(theme)
            .id("finished")
            .debug_selector(move || format!("finished-{}", item.as_uuid()))
            .role(Role::Button)
            .aria_label(label.clone())
            .flex_none()
            .text_color(hsla(quiet))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla_alpha(quiet, alpha::FAINT)))
            .child(ChromeText::new(label, px(theme.typography.small())).fill());
        tab_stop(pill, theme.surfaces.focus)
            .on_click(cx.listener(move |this, _ev, _window, cx| this.reveal_session(session, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use slopty_client::layout::WorkerKey;
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::RequestCard;
    use slopty_proto::thread::{AskId, Liveness, Status as RowStatus};

    use super::*;

    /// A terminal's agent whose row stands at `phase`, waiting on `wait` (kind, words).
    fn at(phase: Phase, wait: Option<(&str, &str)>) -> ThreadStand {
        let rung = match phase {
            Phase::NeedsYou => Rung::NeedsYou,
            Phase::Failed => Rung::Failed,
            Phase::Working => Rung::Working,
            Phase::Waiting => Rung::Waiting,
            Phase::Idle | Phase::Done | Phase::Stopped => Rung::Idle,
        };
        let wait = wait.map(|(kind, text)| Wait { kind: kind.to_owned(), text: text.to_owned() });
        ThreadStand {
            worker: WorkerKey::new(1),
            rung,
            exited: false,
            asks: None,
            terminal: None,
            since: WallMs::ZERO,
            status: Some(RowStatus {
                phase,
                wait,
                liveness: Liveness::Live,
                since_ms: WallMs::ZERO,
            }),
            doing: None,
            resets: None,
            ended: None,
            seen: slopty_proto::thread::TurnId::BEFORE,
        }
    }

    /// The same, with request `kind` open, titled `title`.
    fn asking(kind: &str, title: &str, wait: Option<(&str, &str)>) -> ThreadStand {
        let card = RequestCard {
            id: AskId("a".to_owned()),
            item: None,
            kind: kind.to_owned(),
            title: title.to_owned(),
            options: Vec::new(),
            opened_ms: WallMs::ZERO,
        };
        ThreadStand { asks: Some(card), ..at(Phase::NeedsYou, wait) }
    }

    /// An approval is said as the adapter words its wait, else by the call that waits on it;
    /// a question by its own words. Nothing asked, nothing said.
    #[test]
    fn the_ask_says_what_is_asked_without_the_state() {
        let worded = asking(Request::APPROVAL, "Allow Bash?", Some(("permission", "Run ls")));
        assert_eq!(agent_ask_text(&worded).as_deref(), Some("Run ls"));
        assert_eq!(agent_status_text(&worded), "Needs approval: Run ls");
        let call = ThreadStand {
            doing: Some("$ touch notes.txt".to_owned()),
            ..asking(Request::APPROVAL, "Allow Bash?", None)
        };
        assert_eq!(agent_ask_text(&call).as_deref(), Some("$ touch notes.txt"), "else the call");
        let question = asking(Request::QUESTION, "Which branch?", Some(("question", "Which?")));
        assert_eq!(agent_ask_text(&question).as_deref(), Some("Which branch?"), "its own words");
        assert_eq!(agent_status_text(&question), "Asks: Which branch?");
        let bare = asking(Request::QUESTION, "Has a question", None);
        assert_eq!(agent_status_text(&bare), "Has a question", "said once");
        let held = at(Phase::NeedsYou, Some(("input", "Pick a port")));
        assert_eq!(agent_status_word(&held), "Needs input", "a wait alone says what it asks");
        assert_eq!(agent_status_text(&held), "Needs input: Pick a port");
        let working =
            ThreadStand { doing: Some("Edit src/main.rs".to_owned()), ..at(Phase::Working, None) };
        assert_eq!(agent_ask_text(&working), None, "working asks nothing");
        assert_eq!(agent_status_text(&working), "Edit src/main.rs", "it says the call it is on");
    }

    /// A turn paused on work in the background says what it waits on: the commands it left
    /// running as its thread's header says them, or the work by the hook's words or a count.
    /// It is busy, calmly: not at rest, and nothing asked.
    #[test]
    fn a_paused_turn_says_what_it_waits_on() {
        let cases = [
            (at(Phase::Waiting, Some((Wait::COMMAND, "npm run dev"))), "Running npm run dev"),
            (at(Phase::Waiting, Some((Wait::TASK, "cargo test"))), "Waiting on cargo test"),
            (
                at(Phase::Waiting, Some((Wait::TASK, "2 background tasks"))),
                "Waiting on 2 background tasks",
            ),
            (at(Phase::Waiting, None), "Waiting"),
        ];
        for (agent, text) in cases {
            assert_eq!(agent_status_text(&agent), text);
            assert_eq!(agent_status_word(&agent), "Waiting");
            assert_eq!(agent_mark_of(&agent), Status::Running);
            assert!(!needs_human(&agent) && agent_ask_text(&agent).is_none());
        }
    }

    /// A turn that hit a usage limit says so, and when the limit resets where the plan's windows
    /// say; another failure says the turn failed. The word stays short either way.
    #[test]
    fn a_failed_turn_says_why_and_when_a_limit_resets() {
        let limited = at(Phase::Failed, Some((Wait::LIMIT, "Hit its usage limit")));
        assert_eq!(agent_status_text(&limited), "Hit its usage limit");
        assert_eq!(agent_status_word(&limited), "Limit reached");
        let at_ms = WallMs::now();
        let clock = crate::conversation::figures::stamp(at_ms, at_ms).unwrap_or_default();
        let resets = ThreadStand { resets: Some(at_ms), ..limited };
        assert_eq!(
            agent_status_text(&resets),
            format!("Hit its usage limit \u{b7} resets {clock}")
        );
        let other = at(Phase::Failed, None);
        assert_eq!(agent_status_text(&other), "Turn failed");
        assert_eq!(agent_status_word(&other), "Failed");
    }

    /// The word for a state never carries the detail, so a chip, a pill and a row read one
    /// state the same whatever the agent is doing; a turn over is done, never idle.
    #[test]
    fn a_state_has_one_word_whatever_its_detail() {
        let states = [
            (at(Phase::Idle, None), "Idle"),
            (at(Phase::Working, None), "Working"),
            (asking(Request::APPROVAL, "Bash", None), "Needs approval"),
            (asking(Request::QUESTION, "Which?", None), "Has a question"),
            (asking(Request::ELICITATION, "A port", None), "Needs input"),
            (at(Phase::Done, None), "Turn finished"),
        ];
        for (agent, word) in states {
            let detailed = ThreadStand { doing: Some("$ touch x".to_owned()), ..agent.clone() };
            assert_eq!(agent_status_word(&agent), word);
            assert_eq!(agent_status_word(&detailed), word, "{word}");
        }
        assert_eq!(agent_mark_of(&at(Phase::Done, None)), Status::Done);
        assert_eq!(agent_mark_of(&at(Phase::Idle, None)), Status::Idle);
    }
}
