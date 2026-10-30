//! Coding agents' threads: what their rows in the workers' thread tables say of them, a finished
//! command's badge, the banner when the human is away, and the count of the ones waiting on the
//! human.

use gpui::Context;
use slopty_client::WorkerKey;
use slopty_core::SessionId;
use slopty_kit::icons::Status;
use slopty_proto::thread::Phase;
use slopty_proto::thread::attention::Rung;

use super::attention::About;
use super::faces::ThreadStand;
use super::{Finished, WorkspaceEvent, WorkspaceView};

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

/// A wait on the person for an approval of a call.
const PERMISSION: &str = "permission";

/// The agent's state in a word or two, without the detail: the one word for it wherever a
/// state is said beside something else: a header's pill, a note with nothing asked.
///
/// A tool call is "Working": the word names the state, and which call is the line's to say.
#[must_use]
pub(super) fn agent_status_word(agent: &ThreadStand) -> String {
    match agent_mark_of(agent) {
        Status::NeedsYou => match wait_kind(agent) {
            Some(PERMISSION) => "Needs approval",
            Some("question") => "Has a question",
            Some("input") => "Needs input",
            Some("plan") => "Plan to approve",
            _ => "Needs you",
        },
        Status::Working => "Working",
        Status::Running => "Background work",
        Status::Done => "Turn finished",
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
/// call that waits on it; anything else by its wait's words. A row whose trailing word already says
/// "Needs you" and a chip that says "Needs approval" take this as their detail, so the state is
/// not said twice in one place.
#[must_use]
pub(super) fn agent_ask_text(agent: &ThreadStand) -> Option<String> {
    if !needs_human(agent) {
        return None;
    }
    let call = agent.doing.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let said = match wait_kind(agent) {
        Some(PERMISSION) => wait_text(agent).or(call),
        _ => wait_text(agent),
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
        self.threads_waiting().len()
    }

    /// Agents waiting on the human on one worker.
    #[must_use]
    pub fn needs_you_on(&self, worker: WorkerKey) -> usize {
        self.threads_waiting().iter().filter(|w| w.worker == worker).count()
    }

    /// What some agent is doing changed: tell the app the count (the Dock badge).
    pub(super) fn agents_moved(&self, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::NeedsYou(self.needs_you_count()));
    }

    /// A shell command ended in `session`. Long enough, and with the app away, it is kept as
    /// unseen. No note says it: notes speak for agents.
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
    use slopty_proto::thread::{Liveness, Status as RowStatus, Wait};

    use super::*;

    /// An agent whose row stands at `phase`, waiting on `wait` (kind, words).
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
            since: WallMs::ZERO,
            status: Some(RowStatus {
                phase,
                wait,
                liveness: Liveness::Live,
                since_ms: WallMs::ZERO,
            }),
            doing: None,
            ended: None,
            seen: slopty_proto::thread::TurnId::BEFORE,
        }
    }

    /// An approval is said as the adapter words its wait, else by the call that waits on it;
    /// a question by its own words. Nothing asked, nothing said.
    #[test]
    fn the_ask_says_what_is_asked_without_the_state() {
        let worded = at(Phase::NeedsYou, Some(("permission", "Run ls")));
        assert_eq!(agent_ask_text(&worded).as_deref(), Some("Run ls"));
        assert_eq!(agent_status_word(&worded), "Needs approval");
        let call = ThreadStand {
            doing: Some("$ touch notes.txt".to_owned()),
            ..at(Phase::NeedsYou, Some(("permission", "")))
        };
        assert_eq!(agent_ask_text(&call).as_deref(), Some("$ touch notes.txt"), "else the call");
        let question = at(Phase::NeedsYou, Some(("question", "Which branch?")));
        assert_eq!(agent_ask_text(&question).as_deref(), Some("Which branch?"), "its own words");
        assert_eq!(agent_status_word(&question), "Has a question");
        let held = at(Phase::NeedsYou, Some(("input", "Pick a port")));
        assert_eq!(agent_status_word(&held), "Needs input", "a wait alone says what it asks");
        assert_eq!(agent_ask_text(&held).as_deref(), Some("Pick a port"));
        let working =
            ThreadStand { doing: Some("Edit src/main.rs".to_owned()), ..at(Phase::Working, None) };
        assert_eq!(agent_ask_text(&working), None, "working asks nothing");
    }

    /// The word for a state never carries the detail, so a note and a pill read one state the
    /// same whatever the agent is doing; a turn over is done, never idle; a paused turn waits,
    /// asking nothing.
    #[test]
    fn a_state_has_one_word_whatever_its_detail() {
        let states = [
            (at(Phase::Idle, None), "Idle"),
            (at(Phase::Working, None), "Working"),
            (at(Phase::NeedsYou, Some(("permission", "Bash"))), "Needs approval"),
            (at(Phase::NeedsYou, Some(("question", "Which?"))), "Has a question"),
            (at(Phase::NeedsYou, Some(("input", "A port"))), "Needs input"),
            (at(Phase::NeedsYou, None), "Needs you"),
            (at(Phase::Done, None), "Turn finished"),
            (at(Phase::Waiting, Some((Wait::TASK, "npm run dev"))), "Background work"),
            (at(Phase::Failed, None), "Failed"),
        ];
        for (agent, word) in states {
            let detailed = ThreadStand { doing: Some("$ touch x".to_owned()), ..agent.clone() };
            assert_eq!(agent_status_word(&agent), word);
            assert_eq!(agent_status_word(&detailed), word, "{word}");
        }
        assert_eq!(agent_mark_of(&at(Phase::Done, None)), Status::Done);
        assert_eq!(agent_mark_of(&at(Phase::Idle, None)), Status::Idle);
        let paused = at(Phase::Waiting, Some((Wait::TASK, "cargo test")));
        assert!(!needs_human(&paused) && agent_ask_text(&paused).is_none());
    }
}
