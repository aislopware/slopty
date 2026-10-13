//! Coding agents in terminals and threads: what their rows in the workers' thread tables say of
//! them, a finished command's badge, the banner when the human is away, and the count of the ones
//! waiting on the human.

use gpui::Context;
use slopty_client::WorkerKey;
use slopty_core::SessionId;
use slopty_kit::icons::Status;
use slopty_proto::thread::attention::Rung;
use slopty_proto::thread::{Phase, Request, Wait};

use super::attention::About;
use super::faces::ThreadStand;
use super::{Finished, WorkspaceEvent, WorkspaceView};

/// An agent waiting on the human: where.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Waiting {
    /// The worker it runs on.
    pub worker: WorkerKey,
    /// Its session.
    pub session: SessionId,
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
        Status::Working => agent.doing.as_deref().map_or(word, slopty_kit::markdown::plain_line),
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
    said.map(slopty_kit::markdown::plain_line)
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

    /// Sessions whose agent is waiting on the human, on every worker, by worker.
    pub(super) fn needs_you(&self) -> Vec<Waiting> {
        let mut out: Vec<Waiting> = self
            .agent_sessions()
            .filter(|(_, stand)| needs_human(stand))
            .map(|(session, stand)| Waiting { worker: stand.worker, session })
            .collect();
        out.sort_by_key(|w| (w.worker, w.session));
        out
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

    /// What some agent is doing changed: tell the app the count (the Dock badge).
    pub(super) fn agents_moved(&self, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::NeedsYou(self.needs_you_count()));
    }

    /// A shell command ended in `session`. Long enough, and with the app away, it is kept as
    /// unseen. Neither the bell nor a note says it: they speak for agents.
    pub fn command_finished(&mut self, session: SessionId, done: Finished, cx: &mut Context<Self>) {
        let slow = done.elapsed >= self.slow_command;
        tracing::info!(%session, slow, elapsed = ?done.elapsed, "command finished");
        if self.app_active || !slow {
            return;
        }
        self.finished.insert(About::Session(session), done);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use slopty_client::WorkerKey;
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
            buttons: Vec::new(),
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
