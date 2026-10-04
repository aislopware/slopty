//! Messages the person schedules: sent at a time ([`Delivery::At`]), or once another thread has
//! rested for a while ([`Delivery::After`]).
//!
//! Each is the person's own message, held on the worker beside the thread's other pending ones
//! and kept in its log, so it outlives a restart ([`super::Host::schedule`]). No adapter holds
//! it: the host puts it back in the pending list whatever the adapter tells. At its moment the
//! [`spawn`]ed task sends it the way any client's message goes ([`Fire`]), queued where the
//! agent queues. Withdrawing or editing it before then never reaches the agent ([`act`]).
//!
//! One that was being sent when the worker stopped is never sent again: it comes back held,
//! saying it may have gone ([`CUT_OFF`]), for the person to withdraw or send again.
//!
//! A draft ([`Delivery::Draft`]) is kept the same way with no moment of its own: it goes once
//! the person sends it ([`Intent::Promote`]). A continued thread's first message waits so
//! ([`draft`]).

use std::sync::Arc;
use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::thread::wire::{Intent, Outcome};
use slopty_proto::thread::{
    Cap, Delivery, IntentId, Liveness, Pending, PendingState, Phase, ThreadId, ThreadState,
    TurnState,
};
use tokio::task::JoinHandle;

use super::Host;

/// Why a scheduled message that was going as the worker stopped is held.
pub const CUT_OFF: &str = "It was being sent when the worker stopped, so it may have gone";

/// Sends a scheduled message's words to its thread as `intent` (in the agent's queue, or as a
/// steer where it has none), the way a client's message goes, and says how it went.
pub type Fire = Arc<dyn Fn(ThreadId, IntentId, Intent) -> Outcome + Send + Sync>;

/// A scheduled message whose moment has come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    /// Its thread.
    pub thread: ThreadId,
    /// The intent that scheduled it.
    pub intent: IntentId,
    /// What it sends.
    pub send: Intent,
}

/// Since when `state`'s agent has been at rest: its turn ended, nothing asked of the person,
/// no work of its own under way. `None` while it is not.
#[must_use]
pub fn rested_since(state: &ThreadState) -> Option<WallMs> {
    let working = matches!(state.status.phase, Phase::Working | Phase::Waiting | Phase::NeedsYou)
        || state.last_turn().is_some_and(|turn| turn.state == TurnState::Active)
        || state.open_requests().next().is_some()
        || matches!(state.status.liveness, Liveness::Sleeping { .. });
    (!working).then_some(state.status.since_ms)
}

/// When `pending`, waiting on the worker, goes, with `watched` the thread an
/// [`Delivery::After`] waits on (`None` when it is not held here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    /// Now.
    Now,
    /// At this time, unless what it waits on changes first.
    At(WallMs),
    /// Not until something changes.
    Later,
    /// Never, held for this reason.
    Held(&'static str),
}

/// When `pending` goes, at `now`.
#[must_use]
pub fn when(pending: &Pending, watched: Option<&ThreadState>, now: WallMs) -> When {
    match pending.delivery {
        Delivery::At { at_ms } if at_ms <= now => When::Now,
        Delivery::At { at_ms } => When::At(at_ms),
        Delivery::After { .. } if watched.is_none() => When::Held("There is no such thread here"),
        Delivery::After { settle_ms, .. } => match watched.and_then(rested_since) {
            Some(since) => {
                let at =
                    WallMs::from_millis(since.as_millis().saturating_add(u64::from(settle_ms)));
                if at <= now { When::Now } else { When::At(at) }
            }
            None => When::Later,
        },
        Delivery::Steer | Delivery::Queue | Delivery::Draft | Delivery::Interrupt => When::Later,
    }
}

/// The intent a scheduled message is sent as: one per schedule, apart from the one that
/// scheduled it, which is answered already.
#[must_use]
pub fn sent_as(intent: IntentId) -> IntentId {
    let derived = ThreadId::derived(&["scheduled send", &intent.to_string()]);
    IntentId::from_uuid(*derived.as_uuid())
}

/// Keep `text` on `thread` as a draft for the person to read, change and send, once per
/// intent `id` ([`drafted_as`]); `None` when there is no such thread.
pub fn draft(host: &Host, thread: ThreadId, id: IntentId, text: String) -> Option<Outcome> {
    let draft = drafted_as(id);
    host.schedule(thread, draft, |_, kept| {
        kept.push(Pending {
            intent: draft,
            text,
            attachments: Vec::new(),
            delivery: Delivery::Draft,
            state: PendingState::Waiting,
        });
        Outcome::Accepted
    })
}

/// The intent a draft kept for intent `intent` waits as ([`draft`]).
#[must_use]
pub fn drafted_as(intent: IntentId) -> IntentId {
    let derived = ThreadId::derived(&["draft", &intent.to_string()]);
    IntentId::from_uuid(*derived.as_uuid())
}

/// What comes of `intent` on `thread` when it is about a scheduled message.
///
/// The worker holds those and no adapter knows them: one scheduled (a send with
/// [`Delivery::is_kept`]), or a change to one waiting (withdrawn, its words changed, sent
/// now; never moved, since it goes at its own moment). `None` for any other intent, which
/// goes on to the thread's agent.
pub fn act(host: &Host, thread: ThreadId, id: IntentId, intent: &Intent) -> Option<Outcome> {
    let refused = |reason: &str| Outcome::Refused { reason: reason.to_owned() };
    match intent {
        Intent::Send { text, attachments, delivery } if delivery.is_kept() => {
            let decided = host.schedule(thread, id, |state, scheduled| {
                if !state.meta.can(Cap::SCHEDULE) {
                    return Outcome::Unsupported { cap: Cap::named(Cap::SCHEDULE) };
                }
                if text.trim().is_empty() && attachments.is_empty() {
                    return refused("There is nothing to send");
                }
                if let Err(why) = super::attach::check(attachments) {
                    return refused(&why);
                }
                if let Delivery::After { thread: watched, .. } = delivery
                    && *watched == state.meta.id
                {
                    return refused("A message cannot wait on its own thread");
                }
                scheduled.push(Pending {
                    intent: id,
                    text: text.clone(),
                    attachments: attachments.clone(),
                    delivery: *delivery,
                    state: PendingState::Waiting,
                });
                Outcome::Accepted
            });
            Some(decided.unwrap_or_else(|| refused("There is no such thread here")))
        }
        Intent::Withdraw { pending }
        | Intent::Edit { pending, .. }
        | Intent::Promote { pending }
        | Intent::Reorder { pending, .. }
            if host.is_scheduled(thread, *pending) =>
        {
            let decided = host.schedule(thread, id, |_, scheduled| {
                let Some(at) = scheduled.iter().position(|p| p.intent == *pending) else {
                    return refused("That message has already gone");
                };
                if scheduled.get(at).is_some_and(|p| p.state == PendingState::Sending) {
                    return refused("That message is being sent");
                }
                match intent {
                    Intent::Withdraw { .. } => {
                        scheduled.remove(at);
                    }
                    Intent::Edit { text, .. } => {
                        if let Some(p) = scheduled.get_mut(at) {
                            p.text.clone_from(text);
                        }
                    }
                    // Sent now: its moment is now, and the worker sends it at once.
                    Intent::Promote { .. } => {
                        if let Some(p) = scheduled.get_mut(at) {
                            p.delivery = Delivery::At { at_ms: WallMs::now() };
                            p.state = PendingState::Waiting;
                        }
                    }
                    _ => return refused("A scheduled message goes at its own moment"),
                }
                Outcome::Done
            });
            Some(decided.unwrap_or_else(|| refused("There is no such thread here")))
        }
        _ => None,
    }
}

/// Send each scheduled message in `host` at its moment through `fire`, until the host is gone.
///
/// It looks again whenever a schedule changes or the table does (a thread waited on came to
/// rest), and at the next time one is due.
pub fn spawn(host: Host, fire: Fire) -> JoinHandle<()> {
    tokio::spawn(async move {
        let changed = host.schedule_changed();
        let mut table = host.table_watch();
        loop {
            let (due, next) = host.due(WallMs::now());
            for Due { thread, intent, send } in due {
                let outcome = fire(thread, sent_as(intent), send);
                tracing::info!(%thread, %intent, ?outcome, "a scheduled message went");
                host.fired(thread, intent, &outcome);
            }
            let wait = next.map_or(Duration::MAX, |at| at.since(WallMs::now()));
            tokio::select! {
                () = changed.notified() => {}
                seen = table.changed() => {
                    if seen.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AgentId, Drive, Status, ThreadMeta, Turn, TurnId, Usage};

    use super::*;

    fn state(phase: Phase, since: u64) -> ThreadState {
        let meta = ThreadMeta {
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
        };
        let mut state = ThreadState::new(meta);
        state.status = Status {
            phase,
            wait: None,
            liveness: Liveness::Live,
            since_ms: WallMs::from_millis(since),
        };
        state
    }

    fn pending(delivery: Delivery) -> Pending {
        Pending {
            intent: IntentId::new(),
            text: "next".to_owned(),
            attachments: Vec::new(),
            delivery,
            state: PendingState::Waiting,
        }
    }

    /// A timed message goes at its time; one after another thread goes once that thread has
    /// rested for the settle from when it came to rest, waits while it works, and is held when
    /// the thread is not here.
    #[test]
    fn a_message_goes_at_its_time_or_once_its_thread_has_settled() {
        let ms = WallMs::from_millis;
        let at = pending(Delivery::At { at_ms: ms(5_000) });
        assert_eq!(when(&at, None, ms(4_999)), When::At(ms(5_000)));
        assert_eq!(when(&at, None, ms(5_000)), When::Now);

        let after = pending(Delivery::After { thread: ThreadId::new(), settle_ms: 10_000 });
        let done = state(Phase::Done, 1_000);
        assert_eq!(when(&after, Some(&done), ms(5_000)), When::At(ms(11_000)));
        assert_eq!(when(&after, Some(&done), ms(11_000)), When::Now);
        assert_eq!(when(&after, Some(&state(Phase::Working, 1_000)), ms(99_000)), When::Later);
        assert_eq!(when(&after, Some(&state(Phase::NeedsYou, 1_000)), ms(99_000)), When::Later);
        let mut turning = state(Phase::Idle, 1_000);
        turning.turns.push(Turn {
            id: TurnId(1),
            input: None,
            state: TurnState::Active,
            started_ms: WallMs::ZERO,
            ended_ms: None,
            usage: Usage::default(),
            models: Vec::new(),
            changed: slopty_proto::thread::Changed::default(),
            before: None,
            after: None,
        });
        assert_eq!(when(&after, Some(&turning), ms(99_000)), When::Later, "a turn under way");
        assert_eq!(when(&after, None, ms(1)), When::Held("There is no such thread here"));
        assert_eq!(when(&pending(Delivery::Queue), None, ms(1)), When::Later);
        assert_ne!(sent_as(at.intent), at.intent, "sent apart from its schedule");
        assert_eq!(sent_as(at.intent), sent_as(at.intent), "once");
    }
}
