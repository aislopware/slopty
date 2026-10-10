//! Messages the person schedules: sent at a time ([`Delivery::At`]), as "Continue at" a usage
//! limit's reset.
//!
//! Each is the person's own message, held on the worker beside the thread's other pending ones
//! and kept in its log, so it outlives a restart ([`super::Host::schedule`]). No adapter holds
//! it: the host puts it back in the pending list whatever the adapter tells. At its moment the
//! [`spawn`]ed task sends it the way any client's message goes ([`Fire`]), queued where the
//! agent queues. Withdrawing or editing it before then never reaches the agent ([`act`]).
//!
//! One that was being sent when the worker stopped is never sent again: it comes back held,
//! saying it may have gone ([`CUT_OFF`]), for the person to withdraw or send again.

use std::sync::Arc;
use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::thread::wire::{Intent, Outcome};
use slopty_proto::thread::{Cap, Delivery, IntentId, Pending, PendingState, ThreadId};
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

/// When `pending`, waiting on the worker, goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    /// Now.
    Now,
    /// At this time.
    At(WallMs),
    /// Not at a moment of its own: the agent's queue or a steer sends it.
    Later,
}

/// When `pending` goes, at `now`.
#[must_use]
pub const fn when(pending: &Pending, now: WallMs) -> When {
    match pending.delivery {
        Delivery::At { at_ms } if at_ms.as_millis() <= now.as_millis() => When::Now,
        Delivery::At { at_ms } => When::At(at_ms),
        Delivery::Steer | Delivery::Queue => When::Later,
    }
}

/// The intent a scheduled message is sent as: one per schedule, apart from the one that
/// scheduled it, which is answered already.
#[must_use]
pub fn sent_as(intent: IntentId) -> IntentId {
    let derived = ThreadId::derived(&["scheduled send", &intent.to_string()]);
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
                    _ => {}
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
/// It looks again whenever a schedule changes or the table does, and at the next time one is
/// due; it ends once the table's watch does.
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
    use super::*;

    fn pending(delivery: Delivery) -> Pending {
        Pending {
            intent: IntentId::new(),
            text: "next".to_owned(),
            attachments: Vec::new(),
            delivery,
            state: PendingState::Waiting,
        }
    }

    /// A timed message goes at its time, a queued one never at a moment of its own, and each
    /// is sent apart from the intent that scheduled it, once.
    #[test]
    fn a_message_goes_at_its_time() {
        let ms = WallMs::from_millis;
        let at = pending(Delivery::At { at_ms: ms(5_000) });
        assert_eq!(when(&at, ms(4_999)), When::At(ms(5_000)));
        assert_eq!(when(&at, ms(5_000)), When::Now);
        assert_eq!(when(&pending(Delivery::Queue), ms(1)), When::Later);
        assert_ne!(sent_as(at.intent), at.intent, "sent apart from its schedule");
        assert_eq!(sent_as(at.intent), sent_as(at.intent), "once");
    }
}
