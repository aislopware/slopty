//! The composer of an observed agent: what a person sends a thread whose agent's own TUI runs
//! in a terminal here, typed into that terminal by the worker.
//!
//! It types as a person would: a message as a paste (bracketed where
//! the TUI asked for it), [`SUBMIT_PAUSE`], then Enter; a command (`/model opus`) as typed
//! text, the pause, then Enter; an interrupt as Esc. Every write goes through the guard
//! orchestration's own input does ([`may_type`]): nothing is typed while the agent asks a
//! person something, before its hooks have spoken, once it has exited, or while a person has a
//! line typed and unsent in the terminal. Nothing it types ever clears that line.
//!
//! A message waits in the thread's pending list until it goes, where it can be withdrawn,
//! edited, or promoted to go now as a steer:
//!
//! - a steer goes as soon as the guard lets it, into the turn under way, which the agent takes at
//!   its next step;
//! - a queued one waits for the agent to be at rest, then goes, one per turn: the next waits until
//!   the agent has taken it (it was seen working, or the turn it started ended);
//! - one the guard holds says why ([`DRAFT`]), and goes once the way is clear;
//! - once the person stops the turn, every queued one is held ([`Pending::STOPPED`]) until they
//!   send again, a message or one of those now, which lets the rest go after it in their order.
//!
//! Each thread with something to send has a task of its own, which sends one thing at a time.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_agent::status::{AgentSource, AgentStatus, BlockReason};
use slopty_core::SessionId;
use slopty_proto::orchestration::{ErrorCode, Input};
use slopty_proto::thread::wire::{Intent, Outcome};
use slopty_proto::thread::{
    Action, Delivery, IntentId, ItemBody, Pending, PendingState, ThreadId, ThreadState, TurnState,
};
use tokio::sync::{broadcast, mpsc};

use super::Host;
use crate::orchestrate::{Agents, Failure, may_type, write_input};
use crate::session::SessionHandle;

/// Between the text and the Enter that submits it: a TUI that reads the two in one read takes
/// them as one paste and swallows the Enter.
pub const SUBMIT_PAUSE: Duration = Duration::from_millis(200);

/// The newest items a message that went is looked for among, to see the transcript took it.
const SHOWN_WITHIN: usize = 64;

/// How often a message the guard holds is tried again: a person's draft says nothing when it
/// is sent.
pub const RETRY: Duration = Duration::from_millis(250);

/// Why a message is held while a person's line is typed and unsent in the terminal.
pub const DRAFT: &str = "Your draft in the terminal is in the way";

/// Why a message stays: it was typed in, and the person's draft came in before its Enter.
pub const TYPED_NOT_SENT: &str = "Typed into the terminal but not sent: your draft is in the way";

/// Why a message is back in the list: Claude Code took it back into its input before it began
/// (Esc right after Enter), so it is in the terminal, unsent, and not typed again.
pub const TAKEN_BACK: &str = "Not taken: Claude Code put it back in its input";

/// The terminals the composer types into, and their agents as the guard reads them.
pub trait Terminals: Agents {
    /// The terminal `session`, while it is open.
    fn terminal(&self, session: SessionId) -> Option<SessionHandle>;
}

/// Something typed that is not a message.
#[derive(Debug)]
enum Job {
    /// A command, once the agent is at rest.
    Command(String),
    /// A key, at once.
    Key(&'static str),
}

/// Types what is sent to observed agents into their terminals. Cheap to clone.
#[derive(Clone)]
pub struct Composer {
    host: Host,
    terminals: Arc<dyn Terminals>,
    tasks: Arc<Mutex<HashMap<ThreadId, mpsc::UnboundedSender<Job>>>>,
}

impl std::fmt::Debug for Composer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Composer")
            .field("threads", &self.tasks.lock().len())
            .finish_non_exhaustive()
    }
}

impl Composer {
    /// A composer for the threads of `host`, typing into `terminals`.
    #[must_use]
    pub fn new(host: Host, terminals: Arc<dyn Terminals>) -> Self {
        Self { host, terminals, tasks: Arc::default() }
    }

    /// What intent `id` comes to on the thread `state`, when it is the composer's to act on: a
    /// message, a change to one pending (its words, its place, or its going now), an interrupt
    /// or a model. `None` for any other. Meant
    /// to run once per id, as [`Host::intent`] runs its decision.
    pub fn decide(
        &self,
        state: &ThreadState,
        id: IntentId,
        intent: &Intent,
    ) -> Option<(Outcome, Vec<Action>)> {
        let thread = state.meta.id;
        let refused = |reason: &str| Some((Outcome::Refused { reason: reason.to_owned() }, vec![]));
        match intent {
            Intent::Send { text, delivery, attachments } => {
                if text.trim().is_empty() && attachments.is_empty() {
                    return refused("There is nothing to send");
                }
                let mut pending = state.pending.clone();
                for p in &mut pending {
                    p.release_stop();
                }
                let text = text.clone();
                pending.push(Pending {
                    intent: id,
                    text,
                    attachments: attachments.clone(),
                    delivery: *delivery,
                    state: PendingState::Waiting,
                });
                self.kick(thread);
                Some((Outcome::Accepted, vec![Action::PendingSet(pending)]))
            }
            Intent::Withdraw { pending: which } | Intent::Edit { pending: which, .. } => {
                let mut pending = state.pending.clone();
                let Some(at) = pending.iter().position(|p| p.intent == *which) else {
                    return refused("That message has already gone");
                };
                let typed = PendingState::Held { reason: TYPED_NOT_SENT.to_owned() };
                let back = PendingState::Held { reason: TAKEN_BACK.to_owned() };
                match (intent, pending.get(at).map(|p| &p.state)) {
                    (_, Some(PendingState::Sending)) => {
                        return refused("That message is being typed");
                    }
                    (Intent::Edit { .. }, Some(state)) if *state == typed || *state == back => {
                        return refused("That message is already in the terminal");
                    }
                    (Intent::Edit { text, .. }, Some(_)) => {
                        if let Some(p) = pending.get_mut(at) {
                            p.text.clone_from(text);
                        }
                    }
                    _ => {
                        pending.remove(at);
                    }
                }
                Some((Outcome::Done, vec![Action::PendingSet(pending)]))
            }
            Intent::Promote { pending: which } => {
                let mut pending = state.pending.clone();
                for p in &mut pending {
                    p.release_stop();
                }
                let Some(promoted) = pending.iter_mut().find(|p| p.intent == *which) else {
                    return refused("That message has already gone");
                };
                match &promoted.state {
                    PendingState::Sending => return refused("That message is being typed"),
                    PendingState::Held { reason }
                        if reason == TYPED_NOT_SENT || reason == TAKEN_BACK =>
                    {
                        return refused("That message is already in the terminal");
                    }
                    _ => {}
                }
                promoted.delivery = Delivery::Steer;
                self.kick(thread);
                Some((Outcome::Done, vec![Action::PendingSet(pending)]))
            }
            Intent::Interrupt => {
                let session = state.meta.terminal?;
                let working = self
                    .terminals
                    .status(session)
                    .is_some_and(|a| a.status == AgentStatus::Working);
                if let Err(reason) = self.guard(session) {
                    return refused(&reason);
                }
                if !working {
                    return refused("The agent is not working");
                }
                self.job(thread, Job::Key("escape"));
                // The person's stop holds what is queued; one already in the terminal is not
                // the worker's to hold.
                let mut pending = state.pending.clone();
                let mut held = false;
                for p in pending.iter_mut().filter(|p| !in_terminal(p)) {
                    held |= p.hold_for_stop();
                }
                Some((
                    Outcome::Accepted,
                    if held { vec![Action::PendingSet(pending)] } else { vec![] },
                ))
            }
            Intent::SetModel { model } => {
                if !state.meta.models.iter().any(|m| m.id == *model) {
                    return refused(&format!("There is no model {model} here"));
                }
                self.job(thread, Job::Command(format!("/model {model}")));
                Some((Outcome::Accepted, vec![]))
            }
            _ => None,
        }
    }

    /// Go on with what every thread has waiting, as after a restart of the worker.
    pub fn resume(&self) {
        for thread in self.host.threads() {
            if self.host.update(thread, |s| (vec![], !s.pending.is_empty())) == Some(true) {
                self.kick(thread);
            }
        }
    }

    /// Go on with what `thread` has to send.
    pub fn kick(&self, thread: ThreadId) {
        let _task = self.sender(thread);
    }

    fn job(&self, thread: ThreadId, job: Job) {
        let _gone = self.sender(thread).send(job);
    }

    /// The task of `thread`, started when it has none.
    fn sender(&self, thread: ThreadId) -> mpsc::UnboundedSender<Job> {
        let mut tasks = self.tasks.lock();
        if let Some(tx) = tasks.get(&thread).filter(|tx| !tx.is_closed()) {
            return tx.clone();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tasks.insert(thread, tx.clone());
        drop(tasks);
        tokio::spawn(compose(self.clone(), thread, rx));
        tx
    }

    /// Whether anything may be typed into `session` now; why not, in words, when not.
    fn guard(&self, session: SessionId) -> Result<(), String> {
        let Some(handle) = self.terminals.terminal(session) else {
            return Err("The agent's terminal is closed".to_owned());
        };
        if handle.draft_pending() {
            return Err(DRAFT.to_owned());
        }
        let agents: &dyn Agents = &*self.terminals;
        may_type(&handle, agents, true).map_err(|refused| words(&refused))
    }

    fn at_rest(&self, session: SessionId) -> bool {
        self.terminals.status(session).is_some_and(|agent| {
            agent.source == AgentSource::Hook
                && matches!(
                    agent.status,
                    AgentStatus::Idle
                        | AgentStatus::Done
                        | AgentStatus::Failed { .. }
                        | AgentStatus::Waiting { .. }
                        | AgentStatus::Blocked(BlockReason::IdlePrompt)
                )
        })
    }
}

/// A refusal of the guard, worded for the person.
fn words(refused: &Failure) -> String {
    match refused.code {
        ErrorCode::AwaitsPerson => "Waiting for a request to be answered",
        ErrorCode::AgentNotReady => "Waiting for the agent to be ready",
        ErrorCode::AgentExited => "The agent has exited",
        _ => "The agent's terminal is closed",
    }
    .to_owned()
}

/// What a thread's task does next.
enum Next {
    /// Type a message, now marked as being sent.
    Message { intent: IntentId, text: String, delivery: Delivery },
    /// Nothing can go now; try again in a while when `held`, else when the thread changes.
    Wait { held: bool },
    /// The thread is gone.
    Gone,
}

/// Where a queued message that went stands: it waits for the turn it started to be seen.
#[derive(Clone, Copy)]
struct Started {
    turns: usize,
    working: bool,
}

/// `thread`'s task: one thing at a time, as long as the thread is held.
async fn compose(c: Composer, thread: ThreadId, mut jobs: mpsc::UnboundedReceiver<Job>) {
    let Some(mut feed) = c.host.watch(thread) else { return };
    let mut queued: VecDeque<Job> = VecDeque::new();
    let mut started: Option<Started> = None;
    // The message that went last, until the transcript shows it or Claude Code took it back.
    let mut unconfirmed: Option<Pending> = None;
    loop {
        while let Ok(job) = jobs.try_recv() {
            queued.push_back(job);
        }
        let Some(session) = c.host.update(thread, |s| (vec![], s.meta.terminal)).flatten() else {
            return;
        };
        if let Some(job) = queued.front() {
            let ready = match job {
                Job::Key(_) => true,
                Job::Command(_) => c.at_rest(session) && started.is_none(),
            };
            if ready && let Some(job) = queued.pop_front() {
                type_job(&c, session, job).await;
                continue;
            }
        }
        let next =
            c.host.update(thread, |state| next(&c, state, session, &mut started, &mut unconfirmed));
        let held = match next.unwrap_or(Next::Gone) {
            Next::Gone => return,
            Next::Message { intent, text, delivery } => {
                c.host.typed(thread, intent, &text);
                let left = send(&c, session, &text).await;
                let after = c.host.update(thread, |state| {
                    let went = state.pending.iter().find(|p| p.intent == intent).cloned();
                    let (actions, turns) = sent(state, intent, left.as_deref());
                    (actions, (turns, went))
                });
                if left.is_none() {
                    let (turns, went) = after.unzip();
                    unconfirmed = went.flatten();
                    if delivery == Delivery::Queue {
                        started = turns.map(|turns| Started { turns, working: false });
                    }
                }
                continue;
            }
            Next::Wait { held } => held || !queued.is_empty() || started.is_some(),
        };
        let retry = async {
            if held {
                tokio::time::sleep(RETRY).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            job = jobs.recv() => match job {
                Some(job) => queued.push_back(job),
                None => return,
            },
            batch = feed.recv() => {
                if matches!(batch, Err(broadcast::error::RecvError::Closed)) {
                    return;
                }
            }
            () = retry => {}
        }
    }
}

/// The message to type next in `state`, marked as being sent; else whether one is held.
fn next(
    c: &Composer,
    state: &ThreadState,
    session: SessionId,
    started: &mut Option<Started>,
    unconfirmed: &mut Option<Pending>,
) -> (Vec<Action>, Next) {
    if let Some(back) = taken_back(c, state, session, unconfirmed) {
        return back;
    }
    // A queued message that went holds the next until the agent has taken it: it was seen
    // working since, or the turn the message started has ended.
    if let Some(at) = started.as_mut() {
        at.working |= c.terminals.status(session).is_some_and(|a| a.status == AgentStatus::Working);
        let ended = state.turns.get(at.turns).is_some_and(|t| t.state != TurnState::Active);
        if at.working || ended {
            *started = None;
        }
    }
    let open = |p: &&Pending| p.state != PendingState::Sending && !in_terminal(p) && !p.stopped();
    let steer = state.pending.iter().filter(open).find(|p| p.delivery == Delivery::Steer);
    let queue = state.pending.iter().filter(open).find(|p| p.delivery == Delivery::Queue);
    let resting = started.is_none() && c.at_rest(session);
    let Some(pick) = steer.or_else(|| queue.filter(|_| resting)) else {
        return (vec![], Next::Wait { held: queue.is_some() });
    };
    // Claude Code takes a file by its path, a picture's too: its TUI attaches a picture pasted
    // as a path.
    let text =
        slopty_agent::attach::with_paths(&pick.text, pick.attachments.iter().map(String::as_str));
    let (intent, delivery) = (pick.intent, pick.delivery);
    let now = match c.guard(session) {
        Ok(()) => PendingState::Sending,
        Err(reason) => PendingState::Held { reason },
    };
    let sending = now == PendingState::Sending;
    if pick.state == now {
        return (vec![], Next::Wait { held: true });
    }
    let pending = with_state(&state.pending, intent, &now);
    let next =
        if sending { Next::Message { intent, text, delivery } } else { Next::Wait { held: true } };
    (vec![Action::PendingSet(pending)], next)
}

/// The message that went last, back in the list held for [`TAKEN_BACK`], once Claude Code took
/// it back: the transcript never showed it and the agent is at rest again by its title
/// (`slopty_agent::Tracker`). Forgotten once the transcript shows it.
fn taken_back(
    c: &Composer,
    state: &ThreadState,
    session: SessionId,
    unconfirmed: &mut Option<Pending>,
) -> Option<(Vec<Action>, Next)> {
    let went = unconfirmed.as_ref()?;
    let shown = state.items.iter().rev().take(SHOWN_WITHIN).any(
        |item| matches!(&item.body, ItemBody::User(message) if message.intent == Some(went.intent)),
    );
    if shown {
        *unconfirmed = None;
        return None;
    }
    let back = c.terminals.status(session).is_some_and(|agent| {
        agent.status == AgentStatus::Idle && agent.source == AgentSource::Title
    });
    if !back {
        return None;
    }
    let went = unconfirmed.take()?;
    let held = Pending { state: PendingState::Held { reason: TAKEN_BACK.to_owned() }, ..went };
    let pending = std::iter::once(held).chain(state.pending.iter().cloned()).collect();
    Some((vec![Action::PendingSet(pending)], Next::Wait { held: false }))
}

/// Whether `pending` was typed into the terminal and stays there unsent ([`TYPED_NOT_SENT`],
/// [`TAKEN_BACK`]).
fn in_terminal(pending: &Pending) -> bool {
    matches!(&pending.state, PendingState::Held { reason } if reason == TYPED_NOT_SENT || reason == TAKEN_BACK)
}

/// `pending` with message `intent` in `state`.
fn with_state(pending: &[Pending], intent: IntentId, state: &PendingState) -> Vec<Pending> {
    pending
        .iter()
        .map(|p| {
            if p.intent == intent {
                Pending { state: state.clone(), ..p.clone() }
            } else {
                p.clone()
            }
        })
        .collect()
}

/// Message `intent` went (`left` is `None`) and leaves the list; else it stays, held for
/// `left`. Returns the turns the thread has.
fn sent(state: &ThreadState, intent: IntentId, left: Option<&str>) -> (Vec<Action>, usize) {
    let pending = match left {
        None => state.pending.iter().filter(|p| p.intent != intent).cloned().collect(),
        Some(reason) => {
            with_state(&state.pending, intent, &PendingState::Held { reason: reason.to_owned() })
        }
    };
    (vec![Action::PendingSet(pending)], state.turns.len())
}

/// Type `text` into `session` as a message: `None` once it went, else why it stays.
async fn send(c: &Composer, session: SessionId, text: &str) -> Option<String> {
    let Some(handle) = c.terminals.terminal(session) else {
        return Some("The agent's terminal is closed".to_owned());
    };
    let guard = || guard_input(c, session);
    if let Err(refused) = write_input(&handle, &Input::Paste(text.to_owned()), guard).await {
        return Some(refused.message);
    }
    tokio::time::sleep(SUBMIT_PAUSE).await;
    match write_input(&handle, &Input::Keys(vec!["enter".to_owned()]), guard).await {
        Ok(()) => None,
        Err(_refused) => Some(TYPED_NOT_SENT.to_owned()),
    }
}

/// [`Composer::guard`] as the input path's guard takes it.
fn guard_input(c: &Composer, session: SessionId) -> Result<(), Failure> {
    c.guard(session).map_err(|reason| Failure::new(ErrorCode::AwaitsPerson, reason))
}

/// Type `job` into `session`; what is refused is dropped, and the log says why.
async fn type_job(c: &Composer, session: SessionId, job: Job) {
    let Some(handle) = c.terminals.terminal(session) else { return };
    let guard = || guard_input(c, session);
    let typed = match &job {
        Job::Key(key) => write_input(&handle, &Input::Keys(vec![(*key).to_owned()]), guard).await,
        Job::Command(command) => {
            match write_input(&handle, &Input::Text(command.clone()), guard).await {
                Ok(()) => {
                    tokio::time::sleep(SUBMIT_PAUSE).await;
                    write_input(&handle, &Input::Keys(vec!["enter".to_owned()]), guard).await
                }
                Err(refused) => Err(refused),
            }
        }
    };
    if let Err(refused) = typed {
        tracing::info!(%session, ?job, why = %refused.message, "not typed");
    }
}
