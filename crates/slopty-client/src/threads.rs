//! One worker's threads as this client holds them: the table every list reads, a mirror of each
//! thread open here, and the intents on their way, all kept in step through link drops.
//!
//! [`Threads`] is sans-IO. The app feeds it what the link brings ([`Threads::table`],
//! [`Threads::frame`], [`Threads::done`]) and sends the messages its calls hand back; it reads
//! and writes the [`cache`] off its UI thread.
//!
//! - **The table** ([`TableState`]) follows its own cursor, so the overview is right the moment the
//!   link comes back, with only the rows that changed.
//! - **A mirror per open thread** ([`Mirror`]) applies the worker's frames with the same reducer
//!   the worker runs ([`slopty_proto::thread::ThreadState::apply`]). It is drawn from the cache in
//!   the first frame and caught up from its cursor: after a blink, only the actions missed come.
//! - **The outbox** ([`Outbox`]) holds every intent under the id it was first sent with. It is
//!   drawn at once (a sent message as a pending bubble, an answer flipping its card), sent again
//!   under the same id after a reconnect or a relaunch, and the worker acts on an id once. An
//!   intent stays until the thread's own state shows what it did, so nothing flickers back between
//!   the worker's answer and the agent's record.
//! - **Blobs** ([`Blobs`]): the whole of clipped content, by recency under a byte budget.

pub mod blobs;
pub mod cache;
mod mirror;
mod outbox;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub use blobs::Blobs;
pub use cache::{Cache, Cached};
pub use mirror::Mirror;
use mirror::Took;
pub use outbox::{Outbox, Sent};
use slopty_net::ClientMsg;
use slopty_proto::thread::wire::{
    Expanded, Intent, IntentDone, Review, ReviewScope, TableFrame, ThreadFrame, ThreadRequest,
};
use slopty_proto::thread::{AskId, ContentRef, IntentId, TableState, ThreadId};

/// The turns a snapshot carries when a thread is first followed: enough to fill a tall window;
/// older ones come by page.
pub const SNAPSHOT_TURNS: u32 = 20;

/// How long the worker may gather appends into one frame (`docs/decisions/agents.md`): a frame
/// a display refresh, which a person reads as live.
pub const MAX_LATENCY_MS: u32 = 16;

/// What a frame changed, for the caller to redraw and keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Changed {
    /// Nothing anyone draws.
    Nothing,
    /// The thread's state moved on (and the outbox may have settled).
    Thread,
    /// The whole of some clipped content came.
    Expanded(ContentRef),
    /// The thread's review came ([`Threads::review`]).
    Review,
}

/// One worker's threads on this client.
#[derive(Debug, Default)]
pub struct Threads {
    table: TableState,
    /// A table frame has come since the client started: the cursor means something.
    heard_table: bool,
    mirrors: HashMap<ThreadId, Mirror>,
    outbox: Outbox,
    blobs: Blobs,
    /// Each open thread's last review.
    reviews: HashMap<ThreadId, Arc<Review>>,
    /// Expansions asked for and not come yet.
    asked: HashSet<ContentRef>,
    approvals: bool,
    linked: bool,
    outbox_changed: bool,
}

impl Threads {
    /// Threads with what the cache kept of the outbox: its intents go again once linked.
    #[must_use]
    pub fn new(outbox: Outbox) -> Self {
        Self { outbox, ..Self::default() }
    }

    /// The table.
    #[must_use]
    pub const fn rows(&self) -> &TableState {
        &self.table
    }

    /// The thread open here, as last known.
    #[must_use]
    pub fn mirror(&self, thread: ThreadId) -> Option<&Mirror> {
        self.mirrors.get(&thread)
    }

    /// The threads open here.
    pub fn open(&self) -> impl Iterator<Item = ThreadId> + '_ {
        self.mirrors.keys().copied()
    }

    /// The intents on their way.
    #[must_use]
    pub const fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// The intents for `thread` its state does not show yet: what to draw over it. A sent
    /// message is a pending bubble, an answer flips its card, an interrupt reads "Stopping".
    pub fn unshown(&self, thread: ThreadId) -> impl Iterator<Item = &Sent> {
        let state = self.mirrors.get(&thread).and_then(Mirror::state);
        self.outbox.of(thread).filter(move |s| state.is_none_or(|state| !s.shown_in(state)))
    }

    /// The answer this client gave `ask` that the thread does not show yet: its card is drawn
    /// as answered.
    #[must_use]
    pub fn answering(&self, thread: ThreadId, ask: &AskId) -> Option<&Sent> {
        self.unshown(thread).find(|s| {
            !s.failed()
                && matches!(&s.intent,
                    Intent::Answer { ask: a, .. } | Intent::Release { ask: a } if a == ask)
        })
    }

    /// Whether an interrupt of `thread` is on its way.
    #[must_use]
    pub fn stopping(&self, thread: ThreadId) -> bool {
        self.unshown(thread).any(|s| !s.failed() && matches!(s.intent, Intent::Interrupt))
    }

    /// Whether the link is up.
    #[must_use]
    pub const fn linked(&self) -> bool {
        self.linked
    }

    /// The link came up: what to ask of the worker so everything here catches up from where
    /// it stands. The table from its cursor, each open thread from its own, and every intent
    /// the worker never answered, under the id it first went with.
    pub fn connected(&mut self) -> Vec<ClientMsg> {
        self.linked = true;
        let have = self.heard_table.then_some(self.table.cursor);
        let mut out = vec![ClientMsg::Thread(ThreadRequest::Table { have })];
        if self.approvals {
            out.push(ClientMsg::Thread(ThreadRequest::Approvals { on: true }));
        }
        out.extend(self.mirrors.iter().map(|(thread, mirror)| follow(*thread, mirror)));
        out.extend(self.outbox.unanswered().map(intent_msg));
        out
    }

    /// The link went: every mirror shows what it last knew.
    pub fn disconnected(&mut self) {
        self.linked = false;
        // What was asked and not answered is lost with the link.
        self.asked.clear();
        for mirror in self.mirrors.values_mut() {
            mirror.lost();
        }
    }

    /// Whether this client answers requests ([`ThreadRequest::Approvals`]); what to send.
    pub fn set_approvals(&mut self, on: bool) -> Option<ClientMsg> {
        if self.approvals == on {
            return None;
        }
        self.approvals = on;
        self.linked.then_some(ClientMsg::Thread(ThreadRequest::Approvals { on }))
    }

    /// Open `thread` here, drawn at once from `cached` when the cache kept it; what to send to
    /// follow it. Opening one already open sends nothing.
    pub fn open_thread(&mut self, thread: ThreadId, cached: Option<Cached>) -> Option<ClientMsg> {
        if self.mirrors.contains_key(&thread) {
            return None;
        }
        let mirror = cached.map(|c| Mirror::cached(c.state, c.cursor)).unwrap_or_default();
        let msg = follow(thread, &mirror);
        self.mirrors.insert(thread, mirror);
        self.linked.then_some(msg)
    }

    /// Close `thread` here: what it last was, for the cache, and what to send.
    pub fn close_thread(&mut self, thread: ThreadId) -> (Option<Cached>, Option<ClientMsg>) {
        self.reviews.remove(&thread);
        let cached = self.mirrors.remove(&thread).and_then(|m| cached(&m));
        let msg = self.linked.then_some(ClientMsg::Thread(ThreadRequest::Unfollow { thread }));
        (cached, msg)
    }

    /// What the cache should keep of `thread`.
    #[must_use]
    pub fn to_cache(&self, thread: ThreadId) -> Option<Cached> {
        self.mirrors.get(&thread).and_then(cached)
    }

    /// A frame of the table.
    pub fn table(&mut self, frame: &TableFrame) {
        self.table.apply(frame);
        self.heard_table = true;
        let unopened: Vec<ThreadId> = self
            .outbox
            .all()
            .iter()
            .map(|s| s.thread)
            .filter(|t| !self.mirrors.contains_key(t))
            .collect();
        for thread in unopened {
            self.settle(thread);
        }
    }

    /// Drop the answered intents for `thread` that what is known of it shows.
    fn settle(&mut self, thread: ThreadId) {
        let settled = match self.mirrors.get(&thread).and_then(Mirror::state) {
            Some(state) => self.outbox.settle(thread, state),
            None => self.outbox.settle_by_row(thread, self.table.rows.get(&thread)),
        };
        self.outbox_changed |= settled;
    }

    /// Whether the outbox changed since this was last asked: then the cache keeps it again.
    pub fn take_outbox_changed(&mut self) -> bool {
        std::mem::take(&mut self.outbox_changed)
    }

    /// A frame of `thread`'s stream: what changed, and what to send when the stream must start
    /// again from the mirror's cursor.
    pub fn frame(&mut self, thread: ThreadId, frame: ThreadFrame) -> (Changed, Vec<ClientMsg>) {
        if let ThreadFrame::Expanded { content, body } = frame {
            self.asked.remove(&content);
            self.blobs.put(content.clone(), body);
            return (Changed::Expanded(content), Vec::new());
        }
        if let ThreadFrame::Review(review) = frame {
            if !self.mirrors.contains_key(&thread) {
                return (Changed::Nothing, Vec::new());
            }
            self.reviews.insert(thread, Arc::from(review));
            return (Changed::Review, Vec::new());
        }
        let Some(mirror) = self.mirrors.get_mut(&thread) else {
            return (Changed::Nothing, Vec::new());
        };
        match mirror.take(frame) {
            Took::Moved => {
                self.settle(thread);
                (Changed::Thread, Vec::new())
            }
            Took::Gap => {
                // The worker ignores a follow of a thread it streams, so the stream ends first.
                let restart = vec![
                    ClientMsg::Thread(ThreadRequest::Unfollow { thread }),
                    follow(thread, mirror),
                ];
                (Changed::Nothing, if self.linked { restart } else { Vec::new() })
            }
            Took::Aside => (Changed::Nothing, Vec::new()),
        }
    }

    /// The worker's answer to an intent. Returns whether it was one of this client's.
    pub fn done(&mut self, done: &IntentDone) -> bool {
        let Some(thread) = self.outbox.all().iter().find(|s| s.id == done.id).map(|s| s.thread)
        else {
            return false;
        };
        self.outbox.answered(done);
        self.outbox_changed = true;
        self.settle(thread);
        true
    }

    /// Ask `thread` to do `intent`: its id, and what to send now (nothing while the link is
    /// down; it goes once the link is back).
    pub fn intent(&mut self, thread: ThreadId, intent: Intent) -> (IntentId, Option<ClientMsg>) {
        let sent = Sent { id: IntentId::new(), thread, intent, outcome: None };
        let msg = self.linked.then(|| intent_msg(&sent));
        let id = sent.id;
        self.outbox.push(sent);
        self.outbox_changed = true;
        (id, msg)
    }

    /// Let a failed intent go once the person has read why.
    pub fn dismiss(&mut self, id: IntentId) -> Option<Sent> {
        let gone = self.outbox.dismiss(id);
        self.outbox_changed |= gone.is_some();
        gone
    }

    /// What to send to see what `thread` changed over `scope`; the answer comes as
    /// [`Changed::Review`].
    #[must_use]
    pub fn ask_review(&self, thread: ThreadId, scope: ReviewScope) -> Option<ClientMsg> {
        (self.linked && self.mirrors.contains_key(&thread))
            .then_some(ClientMsg::Thread(ThreadRequest::Review { thread, scope }))
    }

    /// The last review of `thread` that came.
    #[must_use]
    pub fn review(&self, thread: ThreadId) -> Option<&Arc<Review>> {
        self.reviews.get(&thread)
    }

    /// The whole of `content`, when it came.
    pub fn expanded(&mut self, content: &ContentRef) -> Option<Arc<Expanded>> {
        self.blobs.get(content)
    }

    /// What to send to fetch the whole of `content` in `thread`: nothing when it is held, on
    /// its way already, or the link is down.
    pub fn expand(&mut self, thread: ThreadId, content: &ContentRef) -> Option<ClientMsg> {
        if !self.linked || self.blobs.get(content).is_some() || !self.asked.insert(content.clone())
        {
            return None;
        }
        Some(ClientMsg::Thread(ThreadRequest::Expand { thread, content: content.clone() }))
    }

    /// Ask for the turns of `thread` before the first one held.
    #[must_use]
    pub fn page(&self, thread: ThreadId, turns: u32) -> Option<ClientMsg> {
        let state = self.mirrors.get(&thread)?.state()?;
        let before = state.turns.first()?.id;
        (state.older && self.linked).then_some(ClientMsg::Thread(ThreadRequest::Page {
            thread,
            before,
            turns,
        }))
    }
}

const fn follow(thread: ThreadId, mirror: &Mirror) -> ClientMsg {
    ClientMsg::Thread(ThreadRequest::Follow {
        thread,
        have: mirror.cursor(),
        turns: SNAPSHOT_TURNS,
        max_latency_ms: MAX_LATENCY_MS,
    })
}

fn intent_msg(sent: &Sent) -> ClientMsg {
    ClientMsg::Thread(ThreadRequest::Intent {
        id: sent.id,
        thread: sent.thread,
        intent: sent.intent.clone(),
    })
}

fn cached(mirror: &Mirror) -> Option<Cached> {
    Some(Cached { cursor: mirror.cursor()?, state: mirror.state()?.clone() })
}

#[cfg(test)]
mod tests;
