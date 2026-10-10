//! The messages a worker holds for a driven agent until the turn under way ends.
//!
//! Every adapter whose agent takes one message at a time, or whose own queue cannot be edited,
//! keeps the person's queued messages here ([`Cap::QUEUE`](slopty_proto::thread::Cap::QUEUE)):
//! ACP, Codex's app-server and pi's RPC mode. While held, a message can be taken back
//! ([`Queue::withdraw`]), changed ([`Queue::edit`]) or sent at once ([`Queue::take`]), and the
//! thread shows what waits ([`Queue::shown`], `Action::PendingSet`).
//!
//! The person's stop holds everything queued ([`Queue::stop`], [`Pending::STOPPED`]): nothing goes
//! on its own until they speak again, which lets it all go after their words, in its order
//! ([`Queue::release`]). Whether a turn is under way is the adapter's to say, so the next message
//! is asked for only once none is ([`Queue::next_up`]).
//!
//! `X` is what an adapter keeps beside each message, such as the files already read for it.

use std::collections::VecDeque;

use slopty_proto::thread::{Action, Delivery, IntentId, Pending, PendingState};

/// The messages held, first to go first, each with what its adapter keeps beside it.
#[derive(Debug)]
pub struct Queue<X = ()> {
    held: VecDeque<(Pending, X)>,
}

impl<X> Default for Queue<X> {
    fn default() -> Self {
        Self { held: VecDeque::new() }
    }
}

impl Queue {
    /// The queue a thread showed before, as its state keeps it (`ThreadState::pending`): a
    /// thread read again, or its agent started again, holds what it held. A message the worker
    /// keeps for its moment ([`Delivery::is_kept`]) is the worker's, not the queue's, and stays
    /// out.
    #[must_use]
    pub fn of(pending: &[Pending]) -> Self {
        let queued = pending.iter().filter(|p| !p.delivery.is_kept());
        Self { held: queued.map(|p| (p.clone(), ())).collect() }
    }
}

impl<X> Queue<X> {
    /// Hold `text` and the files at `attachments`, sent as intent `intent`, with `kept` beside
    /// it: last, or `first`, before everything held, for a held message promoted past a turn
    /// that stops for it. The action that shows the queue.
    pub fn hold(
        &mut self,
        intent: IntentId,
        text: &str,
        attachments: Vec<String>,
        kept: X,
        first: bool,
    ) -> Action {
        let held = Pending {
            intent,
            text: text.to_owned(),
            attachments,
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        };
        if first {
            self.held.push_front((held, kept));
        } else {
            self.held.push_back((held, kept));
        }
        self.shown()
    }

    /// Take back the message held for `intent`; `None` when none is.
    pub fn withdraw(&mut self, intent: IntentId) -> Option<Action> {
        self.take(intent).map(|_| self.shown())
    }

    /// Make the message held for `intent` say `text`, its files kept; `None` when none is.
    pub fn edit(&mut self, intent: IntentId, text: &str) -> Option<Action> {
        let (held, _) = self.held.iter_mut().find(|(p, _)| p.intent == intent)?;
        text.clone_into(&mut held.text);
        Some(self.shown())
    }

    /// The message held for `intent`, taken off the queue, to go now; `None` when none is.
    pub fn take(&mut self, intent: IntentId) -> Option<(Pending, X)> {
        let at = self.held.iter().position(|(p, _)| p.intent == intent)?;
        self.held.remove(at)
    }

    /// The person's stop: everything queued is held until they speak again. The action that
    /// shows it, when anything was.
    pub fn stop(&mut self) -> Option<Action> {
        let mut held = false;
        for (pending, _) in &mut self.held {
            held |= pending.hold_for_stop();
        }
        held.then(|| self.shown())
    }

    /// The person spoke after their stop: what it held waits for its turn again. Whether
    /// anything was held.
    pub fn release(&mut self) -> bool {
        let mut released = false;
        for (pending, _) in &mut self.held {
            released |= pending.release_stop();
        }
        released
    }

    /// The next message, taken off the queue, unless the person's stop holds it. Asked for
    /// only once no turn is under way.
    pub fn next_up(&mut self) -> Option<(Pending, X)> {
        if self.held.front().is_some_and(|(p, _)| p.stopped()) {
            return None;
        }
        self.held.pop_front()
    }

    /// Everything held, taken out in its order, as the thread is about to be read again
    /// ([`Self::requeue`] puts it back).
    #[must_use]
    pub fn take_all(&mut self) -> Self {
        std::mem::take(self)
    }

    /// `earlier`, taken from the thread as it was before it was read again, held again ahead of
    /// anything held since. The action that shows it, when anything was put back.
    pub fn requeue(&mut self, mut earlier: Self) -> Option<Action> {
        if earlier.held.is_empty() {
            return None;
        }
        earlier.held.append(&mut self.held);
        self.held = earlier.held;
        Some(self.shown())
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The action that shows what is held, in its order.
    #[must_use]
    pub fn shown(&self) -> Action {
        Action::PendingSet(self.held.iter().map(|(p, _)| p.clone()).collect())
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{Action, Delivery, IntentId, Pending, PendingState};

    use super::Queue;

    fn texts(action: &Action) -> Vec<(String, bool)> {
        let Action::PendingSet(pending) = action else { panic!("not the queue: {action:?}") };
        pending.iter().map(|p| (p.text.clone(), p.stopped())).collect()
    }

    /// Held in order, an interrupt's first; taken back, changed and taken to go by intent;
    /// a stop holds all until the person speaks; the next goes only when the stop holds none.
    #[test]
    fn a_queue_holds_in_order_and_a_stop_holds_it_until_the_person_speaks() {
        let (a, b, c) = (IntentId::new(), IntentId::new(), IntentId::new());
        let mut queue = Queue::default();
        let _shown = queue.hold(a, "first", Vec::new(), (), false);
        let _shown = queue.hold(b, "second", Vec::new(), (), false);
        let shown = queue.hold(c, "now", Vec::new(), (), true);
        let order = |q: &Queue| texts(&q.shown()).into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        assert_eq!(order(&queue), ["now", "first", "second"], "an interrupt's goes first");
        assert_eq!(texts(&shown).len(), 3);

        let edited = queue.edit(b, "second, changed").expect("held");
        assert_eq!(texts(&edited)[2].0, "second, changed");
        assert!(queue.withdraw(IntentId::new()).is_none(), "nothing held for it");
        let taken = queue.take(c).expect("held");
        assert_eq!(taken.0.text, "now");

        let stopped = queue.stop().expect("held for the stop");
        assert!(texts(&stopped).iter().all(|(_, s)| *s), "{stopped:?}");
        assert!(queue.next_up().is_none(), "the stop holds it");
        assert!(queue.release(), "the person spoke");
        assert!(!queue.release(), "once");
        assert_eq!(queue.next_up().map(|(p, ())| p.text).as_deref(), Some("first"));
        assert!(queue.withdraw(b).is_some());
        assert!(queue.is_empty());
        assert!(queue.stop().is_none(), "nothing to hold");
    }

    /// A queue taken out while the thread is read again goes back ahead of what came since,
    /// and one restored from a thread's state holds what it showed, less what the worker keeps
    /// for its time.
    #[test]
    fn a_requeue_puts_the_earlier_messages_first_and_a_state_restores_them() {
        let mut queue = Queue::default();
        let _shown = queue.hold(IntentId::new(), "before", Vec::new(), (), false);
        let earlier = queue.take_all();
        assert!(queue.is_empty());
        let _shown = queue.hold(IntentId::new(), "since", Vec::new(), (), false);
        let shown = queue.requeue(earlier).expect("put back");
        let order: Vec<String> = texts(&shown).into_iter().map(|(t, _)| t).collect();
        assert_eq!(order, ["before", "since"]);
        assert!(queue.requeue(Queue::default()).is_none(), "nothing to put back");

        let Action::PendingSet(mut pending) = shown else { panic!("not the queue: {shown:?}") };
        assert!(pending.iter().all(|p: &Pending| p.state == PendingState::Waiting));
        // A message the worker keeps for its time is shown beside the queue but is not of it.
        let at = Delivery::At { at_ms: WallMs::ZERO };
        pending.push(Pending { delivery: at, text: "at noon".to_owned(), ..pending[0].clone() });
        let restored = Queue::of(&pending);
        assert_eq!(texts(&restored.shown()), texts(&queue.shown()), "the kept one stays out");
    }
}
