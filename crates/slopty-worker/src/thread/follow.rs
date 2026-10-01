//! One client following one thread: the frames its stream carries.
//!
//! A [`Follower`] starts with what the client missed since the cursor it holds (the actions,
//! or a snapshot), then sends each batch the thread applies. Within the client's latency
//! budget (`max_latency_ms`) it holds what comes and sends it as one frame, closed early once
//! [`FRAME_TEXT`] bytes of text or [`FRAME_ACTIONS`] actions have gathered, with the appends to
//! one part that come one after another merged into one ([`coalesce`]): the client's state
//! comes out as the batches one by one would leave it. A follower that falls behind the
//! thread's feed, or sees the log start over, catches up from its cursor again.

use std::sync::Arc;
use std::time::Duration;

use slopty_proto::thread::wire::ThreadFrame;
use slopty_proto::thread::{Action, Cursor, ThreadId};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::Instant;

use super::host::{Batch, Host};

/// Bytes of appended text past which a frame goes without waiting out the budget.
pub const FRAME_TEXT: usize = 4 * 1024;

/// Actions past which a frame goes without waiting out the budget.
pub const FRAME_ACTIONS: usize = 512;

/// A client following a thread.
#[derive(Debug)]
pub struct Follower {
    host: Host,
    thread: ThreadId,
    turns: u32,
    budget: Duration,
    cursor: Option<Cursor>,
    feed: Option<broadcast::Receiver<Arc<Batch>>>,
}

impl Follower {
    /// Follow `thread` from `have`, a snapshot carrying its last `turns` turns, holding what
    /// comes for up to `max_latency_ms` to send it together.
    #[must_use]
    pub fn new(
        host: Host,
        thread: ThreadId,
        have: Option<Cursor>,
        turns: u32,
        max_latency_ms: u32,
    ) -> Self {
        Self {
            host,
            thread,
            turns,
            budget: Duration::from_millis(u64::from(max_latency_ms)),
            cursor: have,
            feed: None,
        }
    }

    /// Where the client stands once it has applied the frames sent so far.
    #[must_use]
    pub const fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    /// The next frame, or `None` once the thread is gone.
    pub async fn next(&mut self) -> Option<ThreadFrame> {
        loop {
            let Some(feed) = self.feed.as_mut() else {
                let follow = self.host.follow(self.thread, self.cursor, self.turns)?;
                self.feed = Some(follow.feed);
                let (frame, cursor) = follow.catchup.frame();
                self.cursor = Some(cursor);
                return Some(frame);
            };
            let batch = match feed.recv().await {
                Ok(batch) => batch,
                Err(RecvError::Lagged(_)) => {
                    self.feed = None;
                    continue;
                }
                Err(RecvError::Closed) => return None,
            };
            let Some(mut at) = self.cursor.filter(|c| c.epoch == batch.epoch && c.seq == batch.seq)
            else {
                self.feed = None;
                continue;
            };
            let mut actions = batch.actions.clone();
            let mut text = appended(&actions);
            let deadline = Instant::now().checked_add(self.budget);
            let mut torn = false;
            while text < FRAME_TEXT && actions.len() < FRAME_ACTIONS {
                // A feed that lagged or closed, or a batch that does not follow on (the log
                // started over), is caught up from the cursor on the next call instead.
                let next = match deadline {
                    Some(deadline) if !self.budget.is_zero() => {
                        match tokio::time::timeout_at(deadline, feed.recv()).await {
                            Ok(next) => next.ok(),
                            Err(_elapsed) => break,
                        }
                    }
                    _ => match feed.try_recv() {
                        Err(TryRecvError::Empty) => break,
                        next => next.ok(),
                    },
                };
                let gathered = u64::try_from(actions.len()).unwrap_or(u64::MAX);
                let follows = next.as_ref().is_some_and(|next| {
                    next.epoch == at.epoch && next.seq == at.seq.saturating_add(gathered)
                });
                let Some(next) = next.filter(|_| follows) else {
                    torn = true;
                    break;
                };
                text = text.saturating_add(appended(&next.actions));
                actions.extend(next.actions.iter().cloned());
            }
            let applied = u64::try_from(actions.len()).unwrap_or(u64::MAX);
            let frame = ThreadFrame::Actions {
                epoch: at.epoch,
                first: at.seq,
                next: at.seq.saturating_add(applied),
                actions: coalesce(actions),
            };
            at.seq = at.seq.saturating_add(applied);
            self.cursor = Some(at);
            if torn {
                self.feed = None;
            }
            return Some(frame);
        }
    }
}

fn appended(actions: &[Action]) -> usize {
    actions
        .iter()
        .map(|a| match a {
            Action::Append { text, .. } => text.len(),
            _ => 0,
        })
        .fold(0, usize::saturating_add)
}

/// `actions` with each run of appends to the same part of the same item merged into one,
/// which leaves a state as the run would.
#[must_use]
pub fn coalesce(actions: Vec<Action>) -> Vec<Action> {
    let mut out: Vec<Action> = Vec::with_capacity(actions.len());
    for action in actions {
        if let Action::Append { item, part, text } = &action
            && let Some(Action::Append { item: last_item, part: last_part, text: last_text }) =
                out.last_mut()
            && last_item == item
            && last_part == part
        {
            last_text.push_str(text);
            continue;
        }
        out.push(action);
    }
    out
}
