//! What the activity bar over the composer stacks: the requests waiting on the person, the
//! plan, the files the turn edited, the messages waiting to go, and the work in the
//! background. Nothing here draws.

use slopty_client::threads::{Sent, Threads};
use slopty_proto::thread::detail::{ExecDetail, ExecStatus};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{
    BackgroundTask, Cap, Delivery, IntentId, ItemBody, ItemId, PendingState, Plan, Request,
    ThreadId, ThreadState, ToolDetail, ToolState,
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
    /// Where the files sent with it are on the worker.
    pub attachments: Vec<String>,
    /// When it goes: with the turn, or held until its moment.
    pub delivery: Delivery,
    /// Where it is, in words, when something holds it.
    pub held: Option<String>,
    /// It waits on the person's stop: their next message, or sending it now, lets it go.
    pub stopped: bool,
    /// The worker has it; until then it is on its way from here.
    pub on_worker: bool,
    /// A withdrawal of it is on its way.
    pub withdrawing: bool,
    /// It is going to the agent now, past changing.
    pub going: bool,
    /// This client's last change to it, while the thread does not show it yet.
    pub edit: Option<Edit>,
}

/// A change to a waiting message, on its way or turned down.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Edit {
    /// On its way: the line already reads as it says.
    Sending,
    /// The worker turned it down: the line reads as before, saying why, until the person
    /// dismisses it or edits again.
    Refused {
        /// The edit, to dismiss.
        intent: IntentId,
        /// What it said.
        text: String,
        /// Why, in words.
        reason: String,
    },
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

/// A command run in the background.
#[derive(Clone, Debug)]
pub struct Background<'a> {
    /// The call that started it.
    pub item: &'a ItemId,
    /// What it is, as the call says.
    pub title: &'a str,
    /// How it stands.
    pub detail: &'a ExecDetail,
    /// The end of what it printed.
    pub output: Option<&'a str>,
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
    /// The commands the agent ran in the background: those still running, and those of the
    /// last turn that ended.
    pub background: Vec<Background<'a>>,
    /// The work the agent lists as run in the background that still runs, or ended in the last
    /// turn, what still runs first.
    pub tasks: Vec<&'a BackgroundTask>,
    /// Whether a waiting message can be withdrawn from here.
    pub can_withdraw: bool,
    /// Whether a waiting message can be sent now from here: into the turn under way where the
    /// agent steers, else by stopping the turn and going first. Every agent that holds messages
    /// takes it ([`Intent::Promote`] needs [`Cap::QUEUE`]).
    ///
    /// [`Intent::Promote`]: slopty_proto::thread::wire::Intent::Promote
    pub can_promote: bool,
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
            .map(|p| {
                let mut queued = Queued {
                    intent: p.intent,
                    text: p.text.clone(),
                    attachments: p.attachments.clone(),
                    delivery: p.delivery,
                    held: match &p.state {
                        PendingState::Held { reason } => Some(reason.clone()),
                        PendingState::Waiting | PendingState::Sending => None,
                    },
                    stopped: p.stopped(),
                    on_worker: true,
                    withdrawing: withdrawing.contains(&p.intent),
                    going: matches!(p.state, PendingState::Sending),
                    edit: None,
                };
                // The newest edit of it speaks for it.
                let edit = threads.unshown(thread).filter_map(|s| match &s.intent {
                    Intent::Edit { pending, text } if *pending == p.intent => Some((s, text)),
                    _ => None,
                });
                if let Some((sent, text)) = edit.last() {
                    if let Some(reason) = sent.failure() {
                        queued.edit =
                            Some(Edit::Refused { intent: sent.id, text: text.clone(), reason });
                    } else {
                        queued.text.clone_from(text);
                        queued.edit = Some(Edit::Sending);
                    }
                }
                queued
            })
            .collect();
        queue.extend(threads.unshown(thread).filter_map(|s| match &s.intent {
            Intent::Send { text, delivery, attachments }
                if !s.failed()
                    && matches!(
                        delivery,
                        Delivery::Queue | Delivery::Interrupt | Delivery::At { .. }
                    ) =>
            {
                Some(Queued {
                    intent: s.id,
                    text: text.clone(),
                    attachments: attachments.clone(),
                    delivery: *delivery,
                    held: None,
                    stopped: false,
                    on_worker: false,
                    withdrawing: false,
                    going: false,
                    edit: None,
                })
            }
            _ => None,
        }));
        Self {
            asked,
            plan: state.plan.as_ref().filter(|p| p.steps.iter().any(|s| s.status != STEP_DONE)),
            edited: edited(state),
            // An agent that lists its background work says it better than the calls do.
            background: if state.tasks.is_empty() { background(state) } else { Vec::new() },
            queue,
            tasks: {
                let mut tasks = tasks(state);
                tasks.sort_by_key(|t| !t.is_running());
                tasks
            },
            can_withdraw: state.meta.can(Cap::QUEUE) || state.meta.can(Cap::SCHEDULE),
            can_promote: state.meta.can(Cap::QUEUE),
        }
    }

    /// Whether the bar has anything to show.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.asked.is_empty()
            && self.plan.is_none()
            && self.edited.is_empty()
            && self.queue.is_empty()
            && self.background.is_empty()
            && self.tasks.is_empty()
    }

    /// The requests no answer from here is on its way to: what "2 of 5" counts.
    pub fn waiting(&self) -> impl Iterator<Item = &Asked<'a>> {
        self.asked.iter().filter(|a| a.answered.is_none())
    }
}

/// A plan step's status once done, as agents name it.
pub const STEP_DONE: &str = "completed";

/// The background work the agent lists that still runs, or belongs to the last turn: started
/// by one of its calls, or, started by none, ended since it began. Work of earlier turns that
/// is over has been said by its turn.
fn tasks(state: &ThreadState) -> Vec<&BackgroundTask> {
    let last = state.last_turn();
    state
        .tasks
        .iter()
        .filter(|task| {
            task.is_running()
                || last.is_some_and(|turn| match task.item.as_ref().and_then(|i| state.item(i)) {
                    Some(item) => item.turn == turn.id,
                    None => task.ended_ms.is_some_and(|end| end >= turn.started_ms),
                })
        })
        .collect()
}

/// The commands run in the background that still run, or ended in the last turn.
fn background(state: &ThreadState) -> Vec<Background<'_>> {
    let last = state.last_turn().map(|t| t.id);
    state
        .items
        .iter()
        .filter_map(|item| {
            let ItemBody::Tool(call) = &item.body else { return None };
            let Some(ToolDetail::Exec(exec)) = &call.detail else { return None };
            let running = matches!(exec.status, ExecStatus::Running);
            (exec.background && (running || Some(item.turn) == last)).then(|| Background {
                item: &item.id,
                title: exec.description.as_deref().unwrap_or(&call.title),
                detail: exec,
                output: call.output.as_ref().map(|o| o.text.as_str()),
            })
        })
        .collect()
}

/// The files the last turn's edits and writes changed, each once, with what it did to them.
///
/// Only an edit that was made counts: one still asked for, refused, failed or running has
/// changed nothing yet, so the tray's "Review" would open on nothing.
pub(in crate::conversation::thread) fn edited(state: &ThreadState) -> Vec<Edited> {
    let Some(turn) = state.last_turn() else { return Vec::new() };
    let mut out: Vec<Edited> = Vec::new();
    for item in state.items.iter().rev().take_while(|i| i.turn == turn.id) {
        let ItemBody::Tool(call) = &item.body else { continue };
        if call.state != ToolState::Completed {
            continue;
        }
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
            attachments: vec![],
            delivery: Delivery::Queue,
            state: PendingState::Held {
                reason: "Your draft in the terminal is in the way".to_owned(),
            },
        }];
        let (mut threads, thread) = threads_over(state);
        let (mine, _msg) = threads.intent(
            thread,
            Intent::Send {
                text: "and then".to_owned(),
                delivery: Delivery::Queue,
                attachments: vec![],
            },
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

    /// An edit reads on the line in the frame it was made; a refusal puts the old words back
    /// and says why, until the message reads as the edit says.
    #[test]
    fn a_queued_message_reads_as_its_edit_and_a_refusal_says_why() {
        let mut state = fixtures::empty();
        state.meta.caps = vec![Cap::named(Cap::QUEUE)];
        let waiting = IntentId::new();
        state.pending = vec![Pending {
            intent: waiting,
            text: "after this".to_owned(),
            attachments: vec![],
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        }];
        let (mut threads, thread) = threads_over(state);
        let edit = Intent::Edit { pending: waiting, text: "after that".to_owned() };
        let (refused, _msg) = threads.intent(thread, edit.clone());
        let line = |threads: &Threads| {
            let state = threads.mirror(thread).unwrap().state().unwrap();
            Activity::of(threads, thread, state).queue.first().cloned().unwrap()
        };
        let sending = line(&threads);
        assert_eq!((sending.text.as_str(), &sending.edit), ("after that", &Some(Edit::Sending)));
        let _mine = threads.done(&IntentDone {
            id: refused,
            outcome: Outcome::Refused { reason: "It went".to_owned() },
        });
        let back = line(&threads);
        assert_eq!(back.text, "after this", "the worker's words again");
        assert!(matches!(&back.edit, Some(Edit::Refused { reason, .. }) if reason == "It went"));
        let _gone = threads.dismiss(refused);
        let (done, _msg) = threads.intent(thread, edit);
        let _mine = threads.done(&IntentDone { id: done, outcome: Outcome::Done });
        assert_eq!(line(&threads).text, "after that", "done, until the stream says so");
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

    /// An edit still asked for, or refused, changed nothing: the tray counts only those made.
    #[test]
    fn an_edit_counts_only_once_it_is_made() {
        let edited = |state: ToolState| {
            let mut thread = fixtures::thread("edit");
            for item in &mut thread.items {
                if let ItemBody::Tool(call) = &mut item.body
                    && matches!(call.detail, Some(ToolDetail::Edit(_) | ToolDetail::Write(_)))
                {
                    call.state = state.clone();
                }
            }
            let (threads, thread) = threads_over(thread);
            let state = threads.mirror(thread).unwrap().state().unwrap();
            Activity::of(&threads, thread, state).edited.len()
        };
        assert!(edited(ToolState::Completed) > 0, "the recorded session edits a file");
        let asked = ToolState::Pending { ask: AskId("a".to_owned()) };
        for state in [asked, ToolState::Running, ToolState::Rejected, ToolState::Failed] {
            assert_eq!(edited(state.clone()), 0, "{state:?}");
        }
    }
}
