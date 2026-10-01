//! What the activity bar over the composer stacks: the requests waiting on the person, the
//! plan, the files the turn edited, the messages waiting to go, and the work in the
//! background. Nothing here draws.

use slopty_client::threads::{Sent, Threads};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{
    BackgroundTask, Cap, Delivery, IntentId, ItemBody, PendingState, Plan, Request, ThreadId,
    ThreadState, ToolDetail,
};

/// A request as the bar shows it.
#[derive(Clone, Debug)]
pub struct Asked<'a> {
    /// The request.
    pub request: &'a Request,
    /// The answer this client gave that the thread does not show yet: the card is drawn as
    /// answered, in the frame the person answered.
    pub answered: Option<&'a Sent>,
}

/// A message waiting to go.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Queued {
    /// The intent that sent it.
    pub intent: IntentId,
    /// What it says.
    pub text: String,
    /// Where it is, in words, when something holds it.
    pub held: Option<String>,
    /// The worker has it; until then it is on its way from here.
    pub on_worker: bool,
    /// A withdrawal of it is on its way.
    pub withdrawing: bool,
}

/// A file the turn edited.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Edited {
    /// Its path, as the agent gave it.
    pub path: String,
    /// Lines added.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
}

/// What the bar stacks, top to bottom.
#[derive(Clone, Debug, Default)]
pub struct Activity<'a> {
    /// The requests open, in the order they opened.
    pub asked: Vec<Asked<'a>>,
    /// The plan, while it has steps not done.
    pub plan: Option<&'a Plan>,
    /// The files the last turn edited, each once.
    pub edited: Vec<Edited>,
    /// The messages waiting to go, in their order.
    pub queue: Vec<Queued>,
    /// The work running in the background.
    pub tasks: Vec<&'a BackgroundTask>,
    /// Whether a task can be stopped from here.
    pub can_stop: bool,
    /// Whether a waiting message can be withdrawn from here.
    pub can_withdraw: bool,
}

impl<'a> Activity<'a> {
    /// What `thread` shows in the bar, over this client's intents on their way.
    #[must_use]
    pub fn of(threads: &'a Threads, thread: ThreadId, state: &'a ThreadState) -> Self {
        let asked = state
            .open_requests()
            .map(|request| Asked { request, answered: threads.answering(thread, &request.id) })
            .collect();
        let withdrawing: Vec<IntentId> = threads
            .unshown(thread)
            .filter_map(|s| match s.intent {
                Intent::Withdraw { pending } if !s.failed() => Some(pending),
                _ => None,
            })
            .collect();
        let mut queue: Vec<Queued> = state
            .pending
            .iter()
            .map(|p| Queued {
                intent: p.intent,
                text: p.text.clone(),
                held: match &p.state {
                    PendingState::Held { reason } => Some(reason.clone()),
                    PendingState::Waiting | PendingState::Sending => None,
                },
                on_worker: true,
                withdrawing: withdrawing.contains(&p.intent),
            })
            .collect();
        queue.extend(threads.unshown(thread).filter_map(|s| match &s.intent {
            Intent::Send { text, delivery: Delivery::Queue } if !s.failed() => Some(Queued {
                intent: s.id,
                text: text.clone(),
                held: None,
                on_worker: false,
                withdrawing: false,
            }),
            _ => None,
        }));
        Self {
            asked,
            plan: state.plan.as_ref().filter(|p| p.steps.iter().any(|s| s.status != STEP_DONE)),
            edited: edited(state),
            queue,
            tasks: state.tasks.iter().filter(|t| t.state == TASK_RUNNING).collect(),
            can_stop: state.meta.can(Cap::STOP_TASK),
            can_withdraw: state.meta.can(Cap::QUEUE),
        }
    }

    /// Whether the bar has anything to show.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.asked.is_empty()
            && self.plan.is_none()
            && self.edited.is_empty()
            && self.queue.is_empty()
            && self.tasks.is_empty()
    }

    /// The requests no answer from here is on its way to: what "2 of 5" counts.
    pub fn waiting(&self) -> impl Iterator<Item = &Asked<'a>> {
        self.asked.iter().filter(|a| a.answered.is_none())
    }
}

/// A plan step's status once done, as agents name it.
pub const STEP_DONE: &str = "completed";

/// A background task's state while it runs, as agents name it.
pub const TASK_RUNNING: &str = "running";

/// The files the last turn's edits and writes changed, each once, with what it did to them.
fn edited(state: &ThreadState) -> Vec<Edited> {
    let Some(turn) = state.last_turn() else { return Vec::new() };
    let mut out: Vec<Edited> = Vec::new();
    for item in state.items.iter().rev().take_while(|i| i.turn == turn.id) {
        let ItemBody::Tool(call) = &item.body else { continue };
        let (path, patch) = match &call.detail {
            Some(ToolDetail::Edit(d)) => (&d.path, &d.patch),
            Some(ToolDetail::Write(d)) => (&d.path, &d.patch),
            _ => continue,
        };
        match out.iter_mut().find(|e| e.path == *path) {
            Some(e) => {
                e.added = e.added.saturating_add(patch.added);
                e.removed = e.removed.saturating_add(patch.removed);
            }
            None => {
                out.push(Edited { path: path.clone(), added: patch.added, removed: patch.removed });
            }
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::{IntentDone, Outcome, ThreadFrame};
    use slopty_proto::thread::{
        Action, AskId, Cursor, Delivery, Pending, PendingState, Request, RequestState,
    };

    use super::*;
    use crate::conversation::thread::fixtures;

    fn request(id: &str) -> Request {
        Request {
            id: AskId(id.to_owned()),
            item: None,
            kind: Request::APPROVAL.to_owned(),
            title: format!("Run {id}?"),
            text: None,
            options: Vec::new(),
            questions: Vec::new(),
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: WallMs::ZERO,
            until_ms: None,
        }
    }

    fn threads_over(state: ThreadState) -> (Threads, ThreadId) {
        let thread = state.meta.id;
        let mut threads = Threads::default();
        let _sent = threads.connected();
        let _follow = threads.open_thread(thread, None);
        let snapshot =
            ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 0 }, state: Box::new(state) };
        let _took = threads.frame(thread, snapshot);
        (threads, thread)
    }

    #[test]
    fn stacked_requests_count_only_those_still_waiting_on_the_person() {
        let mut state = fixtures::empty();
        state.meta.caps = vec![Cap::named(Cap::APPROVALS)];
        for id in ["a", "b", "c"] {
            state.apply(&Action::RequestOpened(Box::new(request(id))));
        }
        let (mut threads, thread) = threads_over(state);
        let answer = Intent::Answer {
            ask: AskId("a".to_owned()),
            choice: "allow".to_owned(),
            message: None,
        };
        let (_id, _msg) = threads.intent(thread, answer);
        let state = threads.mirror(thread).unwrap().state().unwrap();
        let bar = Activity::of(&threads, thread, state);
        assert_eq!(bar.asked.len(), 3);
        assert!(
            bar.asked.first().unwrap().answered.is_some(),
            "flipped in the frame it was answered"
        );
        assert_eq!(bar.waiting().count(), 2, "\"1 of 2\" from here on");
    }

    #[test]
    fn the_queue_holds_the_worker_s_and_this_client_s_on_their_way() {
        let mut state = fixtures::empty();
        state.meta.caps = vec![Cap::named(Cap::QUEUE)];
        let held = IntentId::new();
        state.pending = vec![Pending {
            intent: held,
            text: "after this".to_owned(),
            delivery: Delivery::Queue,
            state: PendingState::Held {
                reason: "Your draft in the terminal is in the way".to_owned(),
            },
        }];
        let (mut threads, thread) = threads_over(state);
        let (mine, _msg) = threads.intent(
            thread,
            Intent::Send { text: "and then".to_owned(), delivery: Delivery::Queue },
        );
        let (withdraw, _msg) = threads.intent(thread, Intent::Withdraw { pending: held });
        let state = threads.mirror(thread).unwrap().state().unwrap();
        let bar = Activity::of(&threads, thread, state);
        let queue: Vec<(IntentId, bool, bool, bool)> = bar
            .queue
            .iter()
            .map(|q| (q.intent, q.held.is_some(), q.on_worker, q.withdrawing))
            .collect();
        assert_eq!(queue, [(held, true, true, true), (mine, false, false, false)]);
        assert!(bar.can_withdraw);
        let _mine = threads.done(&IntentDone {
            id: withdraw,
            outcome: Outcome::Refused { reason: "Gone".to_owned() },
        });
        let state = threads.mirror(thread).unwrap().state().unwrap();
        let bar = Activity::of(&threads, thread, state);
        assert!(!bar.queue.first().unwrap().withdrawing, "a refused withdrawal puts it back");
    }

    #[test]
    fn the_edited_files_are_the_last_turn_s_each_once() {
        let state = fixtures::thread("edit");
        let (threads, thread) = threads_over(state);
        let state = threads.mirror(thread).unwrap().state().unwrap();
        let bar = Activity::of(&threads, thread, state);
        assert!(!bar.edited.is_empty(), "the recorded session edits a file");
        let mut paths: Vec<&str> = bar.edited.iter().map(|e| e.path.as_str()).collect();
        paths.dedup();
        assert_eq!(paths.len(), bar.edited.len());
    }
}
