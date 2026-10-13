//! Agents' turns that ended while nobody looked: which are left for the person to review. They
//! are what the navigator's *To review* lists and what the bell counts beside the agents that
//! need the person, those that failed and the tasks ready to merge.
//!
//! What is unread is the worker's word, not this device's: each thread's row carries its latest
//! turn that ended and the person's seen mark, set by whichever device looked
//! ([`Intent::Seen`]). So a turn read on the phone leaves the Mac's bell, and a turn that ended
//! while this app was not running is unread here when it next hears the worker. A chat the
//! person opens in the shell (`markChatSeen`) marks its thread seen; the mark shows here at once
//! and goes to the worker, which tells every other device.

use std::collections::HashMap;
use std::time::Duration;

use gpui::Context;
use slopty_client::layout::WorkerKey;
use slopty_core::SessionId;
use slopty_proto::thread::wire::{Intent, TurnEnded};
use slopty_proto::thread::{ThreadId, TurnId};

use super::attention::About;
use super::{AgentTurn, Finished, WorkspaceView};

/// What an agent's finished turn says when its worker gave no words of the turn's own.
pub(super) const TURN_FINISHED: &str = "Turn finished";

/// The seen marks this device has set and its workers have not yet said back.
#[derive(Debug, Default)]
pub(super) struct Turns {
    /// The turn each thread was seen through here, ahead of its row's mark: shown at once,
    /// let go once the row says as much.
    seen: HashMap<ThreadId, TurnId>,
}

impl WorkspaceView {
    /// What the bell counts: the agents and threads that need the person, those whose turn
    /// failed, those whose changes wait to be reviewed, and the agents' turns left unread. A
    /// shell's finish is not counted.
    #[must_use]
    pub fn bell_count(&self) -> usize {
        use slopty_proto::thread::attention::Rung;
        let on = |rung: Rung| {
            let sessions = self.agent_sessions().filter(|(_, stand)| stand.rung == rung).count();
            sessions.saturating_add(self.threads_on(rung).len())
        };
        self.needs_you_count()
            .saturating_add(on(Rung::Failed))
            .saturating_add(on(Rung::ToReview))
            .saturating_add(self.agent_turns().count())
    }

    /// The turn `thread` is seen through: its row's mark, or this device's own ahead of it.
    fn seen_through(&self, thread: ThreadId, row: TurnId) -> TurnId {
        self.turns.seen.get(&thread).map_or(row, |here| row.max(*here))
    }

    /// `key`'s table came: each of its threads whose latest turn the agent answered, long
    /// enough to be worth a word, and past the person's seen mark is unread: it earns a header
    /// badge, a row under *To review*, a count on the bell, the tile's unseen dot and, if it
    /// ended while this device heard the worker, a note while the app is away
    /// ([`super::attention::Look::turns`]). One no longer unread (seen on another device, or a
    /// new turn under way) leaves them all. `first` is the worker's first table here.
    pub(super) fn turns_heard(&mut self, key: WorkerKey, first: bool, cx: &mut Context<Self>) {
        let stands: Vec<(ThreadId, Option<SessionId>, Option<TurnEnded>, TurnId)> = self
            .table_stands(key)
            .map(|(id, stand)| (id, stand.terminal, stand.ended, stand.seen))
            .collect();
        let mut moved = false;
        for (thread, terminal, ended, row_seen) in stands {
            if self.turns.seen.get(&thread).is_some_and(|here| *here <= row_seen) {
                self.turns.seen.remove(&thread);
            }
            let about = terminal.map_or(About::Thread(thread), About::Session);
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

    /// What an unread finish of is an agent's turn: a terminal's, or a thread's its worker's
    /// table still holds.
    pub(super) fn agent_turns(&self) -> impl Iterator<Item = About> + '_ {
        self.finished.iter().filter(|(_, done)| done.turn.is_some()).map(|(a, _)| *a).filter(
            |about| match about {
                About::Session(_) => true,
                About::Thread(thread) => self.thread_stand(*thread).is_some(),
            },
        )
    }
}
