//! A thread's agent put to sleep on the person's word, and woken.
//!
//! Asleep, the agent's process is ended the way the agent ends on its own (pi's and an ACP
//! agent's stdin closed, Codex letting the thread go, Claude Code's terminal closed) and the
//! thread is kept with its session. A wake, or the next message, takes the session up again
//! through the agent's own resume. Only an agent at rest with nothing of its own under way is
//! put to sleep ([`refusal`]): ending it then loses nothing the session does not keep.

use slopty_proto::thread::{BackgroundTask, Liveness, Phase, ThreadState, TurnState};

/// Why `state`'s agent is not put to sleep now, in words; `None` when it may be. `armed` says
/// whether a message is scheduled for the thread, which a sleeping agent would miss.
#[must_use]
pub fn refusal(state: &ThreadState, armed: bool) -> Option<&'static str> {
    let at_rest = state.last_turn().is_none_or(|turn| turn.state != TurnState::Active)
        && !matches!(state.status.phase, Phase::Working | Phase::Waiting);
    match state.status.liveness {
        Liveness::Asleep { .. } => Some("It is asleep already"),
        Liveness::Exited { .. } => Some("Its agent is not running"),
        Liveness::Sleeping { .. } => Some("It waits on a wakeup of its own"),
        Liveness::Live | Liveness::Silent { .. } => {
            if state.turns.is_empty() {
                Some("It has not been asked anything yet")
            } else if state.open_requests().next().is_some()
                || state.status.phase == Phase::NeedsYou
            {
                Some("It waits on your answer")
            } else if !at_rest {
                Some("It is working")
            } else if !state.pending.is_empty() {
                Some("A message waits to go to it")
            } else if state.tasks.iter().any(BackgroundTask::is_running) {
                Some("Its background work still runs")
            } else if armed {
                Some("A scheduled message waits for it")
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{
        AgentId, AskId, BackgroundTask, Delivery, Drive, IntentId, Pending, PendingState, Request,
        RequestState, Status, ThreadId, ThreadMeta, Turn, TurnId, Usage,
    };

    use super::*;

    fn meta() -> ThreadMeta {
        ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::PI),
            agent_version: String::new(),
            native: "s".to_owned(),
            cwd: "/w".to_owned(),
            title: String::new(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::DRIVEN),
            caps: Vec::new(),
            models: Vec::new(),
            modes: Vec::new(),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        }
    }

    fn turn(state: TurnState) -> Turn {
        Turn {
            id: TurnId(1),
            input: None,
            state,
            started_ms: WallMs::ZERO,
            ended_ms: None,
            usage: Usage::default(),
            models: Vec::new(),
            changed: slopty_proto::thread::Changed::default(),
            before: None,
            after: None,
        }
    }

    fn request(state: RequestState) -> Request {
        Request {
            id: AskId("ask-1".to_owned()),
            item: None,
            kind: Request::APPROVAL.to_owned(),
            title: "Run cargo test?".to_owned(),
            text: None,
            options: Vec::new(),
            questions: Vec::new(),
            proposed: None,
            editable: Vec::new(),
            schema_json: None,
            url: None,
            state,
            opened_ms: WallMs::ZERO,
            until_ms: None,
        }
    }

    /// At rest after a turn, with nothing of its own under way.
    fn resting() -> ThreadState {
        let mut state = ThreadState::new(meta());
        state.turns.push(turn(TurnState::Complete));
        state.status = Status { phase: Phase::Done, ..Status::default() };
        state
    }

    /// Only an agent that runs, was asked something and rests with nothing waiting on it or
    /// under way is put to sleep; each other case says why.
    #[test]
    fn only_an_agent_at_rest_with_nothing_under_way_sleeps() {
        assert_eq!(refusal(&resting(), false), None);

        let mut never = ThreadState::new(meta());
        never.status.phase = Phase::Idle;
        assert_eq!(refusal(&never, false), Some("It has not been asked anything yet"));

        let mut working = resting();
        working.turns[0].state = TurnState::Active;
        assert_eq!(refusal(&working, false), Some("It is working"));
        let mut waiting = resting();
        waiting.status.phase = Phase::Waiting;
        assert_eq!(refusal(&waiting, false), Some("It is working"));

        let mut asking = resting();
        asking.status.phase = Phase::NeedsYou;
        assert_eq!(refusal(&asking, false), Some("It waits on your answer"));
        let mut open = resting();
        open.requests.push(request(RequestState::Open));
        assert_eq!(refusal(&open, false), Some("It waits on your answer"));
        let mut settled = resting();
        settled.requests.push(request(RequestState::Withdrawn));
        assert_eq!(refusal(&settled, false), None, "a settled request holds nothing");

        let mut queued = resting();
        queued.pending.push(Pending {
            intent: IntentId::new(),
            text: "next".to_owned(),
            attachments: Vec::new(),
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        });
        assert_eq!(refusal(&queued, false), Some("A message waits to go to it"));

        let mut shell = resting();
        let mut task = BackgroundTask {
            id: "b1".to_owned(),
            kind: BackgroundTask::SHELL.to_owned(),
            title: "npm run dev".to_owned(),
            state: BackgroundTask::RUNNING.to_owned(),
            item: None,
            output: None,
            started_ms: WallMs::ZERO,
            ended_ms: None,
        };
        shell.tasks.push(task.clone());
        assert_eq!(refusal(&shell, false), Some("Its background work still runs"));
        BackgroundTask::COMPLETED.clone_into(&mut task.state);
        shell.tasks = vec![task];
        assert_eq!(refusal(&shell, false), None, "work that ended holds nothing");

        assert_eq!(refusal(&resting(), true), Some("A scheduled message waits for it"));

        let mut gone = resting();
        gone.status.liveness = Liveness::Exited { resumable: true };
        assert_eq!(refusal(&gone, false), Some("Its agent is not running"));
        let mut asleep = resting();
        asleep.status.liveness = Liveness::Asleep { since_ms: WallMs::ZERO };
        assert_eq!(refusal(&asleep, false), Some("It is asleep already"));
        let mut wakeup = resting();
        wakeup.status.liveness = Liveness::Sleeping { until_ms: WallMs::ZERO };
        assert_eq!(refusal(&wakeup, false), Some("It waits on a wakeup of its own"));
    }
}
