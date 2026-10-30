//! Agents' turns that ended while nobody looked: which are left for the person to review, and
//! what a note says while their session is in the background (`attention`). The icon badge
//! leaves them out: it counts what needs the person and what failed.
//!
//! What is unread is the worker's word, not this device's: each thread's row carries its latest
//! turn that ended and the person's seen mark, set by whichever device looked
//! ([`Intent::Seen`]). So a turn read on the phone is read on the Mac, and a turn that ended
//! while this app was not running is unread here when it next hears the worker. A chat the
//! person opens in the shell (`markChatSeen`) marks its thread seen; the mark shows here at once
//! and goes to the worker, which tells every other device.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::Context;
use slopty_client::WorkerKey;
use slopty_proto::thread::attention::Rung;
use slopty_proto::thread::wire::{Intent, TurnEnded};
use slopty_proto::thread::{ThreadId, TurnId};

use super::attention::About;
use super::{AgentTurn, Finished, WorkspaceView};

/// What an agent's finished turn says when its worker gave no words of the turn's own.
pub(super) const TURN_FINISHED: &str = "Turn finished";

/// The seen marks this device has set and its workers have not yet said back, and the waits it
/// found already under way.
#[derive(Debug, Default)]
pub(super) struct Turns {
    /// The turn each thread was seen through here, ahead of its row's mark: shown at once,
    /// let go once the row says as much.
    seen: HashMap<ThreadId, TurnId>,
    /// The threads already waiting on the person when their worker was first heard here, let
    /// go once they stop: their wait is no moment, so no note says it (`attention`).
    waited_when_heard: HashSet<ThreadId>,
}

impl WorkspaceView {
    /// Whether `thread` was already waiting on the person when its worker was first heard here,
    /// and waits still.
    pub(super) fn waited_when_heard(&self, thread: ThreadId) -> bool {
        self.turns.waited_when_heard.contains(&thread)
    }

    /// The turn `thread` is seen through: its row's mark, or this device's own ahead of it.
    pub(super) fn seen_through(&self, thread: ThreadId, row: TurnId) -> TurnId {
        self.turns.seen.get(&thread).map_or(row, |here| row.max(*here))
    }

    /// `key`'s table came: each of its threads whose latest turn the agent answered, long
    /// enough to be worth a word, and past the person's seen mark is unread: it earns a header
    /// badge, a row under *To review*, the tile's unseen dot and, if it ended while this
    /// device heard the worker, a note while its session is in the background
    /// ([`super::attention::Look::turns`]). One no longer unread (seen on another device, or a
    /// new turn under way) leaves them all. `first` is the worker's first table here: a thread
    /// waiting on the person in it waited before this device heard it
    /// ([`Self::waited_when_heard`]).
    pub(super) fn turns_heard(&mut self, key: WorkerKey, first: bool, cx: &mut Context<Self>) {
        let stands: Vec<(ThreadId, Option<TurnEnded>, TurnId, Rung)> = self
            .table_stands(key)
            .map(|(id, stand)| (id, stand.ended, stand.seen, stand.rung))
            .collect();
        let mut moved = false;
        for (thread, ended, row_seen, rung) in stands {
            if rung != Rung::NeedsYou {
                self.turns.waited_when_heard.remove(&thread);
            } else if first {
                self.turns.waited_when_heard.insert(thread);
            }
            if self.turns.seen.get(&thread).is_some_and(|here| *here <= row_seen) {
                self.turns.seen.remove(&thread);
            }
            let about = About::Thread(thread);
            let seen = self.seen_through(thread, row_seen);
            let unread = ended.filter(|e| {
                e.answered && e.turn > seen && Duration::from_millis(e.ran_ms) >= self.slow_command
            });
            let held = self.finished.get(&about).and_then(|f| f.turn);
            match unread {
                Some(e) if held.is_none_or(|h| h.thread != thread || h.turn != e.turn) => {
                    let command = self
                        .thread_line(thread)
                        .map(str::to_owned)
                        .filter(|d| !d.trim().is_empty())
                        .unwrap_or_else(|| TURN_FINISHED.to_owned());
                    let turn = AgentTurn { thread, turn: e.turn, fresh: !first };
                    let elapsed = Duration::from_millis(e.ran_ms);
                    self.finished
                        .insert(about, Finished { command, exit: None, elapsed, turn: Some(turn) });
                    moved = true;
                }
                Some(_) => {}
                None => moved |= held.is_some() && self.finished.remove(&about).is_some(),
            }
        }
        let gone: Vec<ThreadId> = self
            .turns
            .waited_when_heard
            .iter()
            .filter(|thread| self.thread_stand(**thread).is_none())
            .copied()
            .collect();
        for thread in gone {
            self.turns.waited_when_heard.remove(&thread);
        }
        if moved {
            cx.notify();
        }
    }

    /// Set `thread`'s seen mark through `turn`: held here until its row says it back, and sent
    /// to `key`'s worker, which carries it to every device.
    pub(super) fn mark_seen(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        turn: TurnId,
        cx: &mut Context<Self>,
    ) {
        self.turns.seen.insert(thread, turn);
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| {
            let _id = hub.intent(thread, Intent::Seen { turn }, cx);
        });
    }

    /// The threads whose unread finish is an agent's turn its worker's table still holds.
    pub(super) fn agent_turns(&self) -> impl Iterator<Item = ThreadId> + '_ {
        self.finished.iter().filter(|(_, done)| done.turn.is_some()).filter_map(|(about, _)| {
            let About::Thread(thread) = *about else { return None };
            self.thread_stand(thread).is_some().then_some(thread)
        })
    }
}
