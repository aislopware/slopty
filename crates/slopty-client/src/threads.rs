//! One worker's threads as this client holds them: the table every list reads, a mirror of each
//! thread open here, and the intents on their way, all kept in step through link drops.
//!
//! [`Threads`] is sans-IO. The app feeds it what the link brings ([`Threads::table`],
//! [`Threads::frame`], [`Threads::done`]) and sends the messages its calls hand back; it reads
//! and writes the [`cache`] off its UI thread.
//!
//! - **The table** ([`TableState`]) follows its own cursor, so every list of threads is right the
//!   moment the link comes back, with only the rows that changed.
//! - **A mirror per open thread** ([`Mirror`]) applies the worker's frames with the same reducer
//!   the worker runs ([`slopty_proto::thread::ThreadState::apply`]). It is drawn from the cache in
//!   the first frame and caught up from its cursor: after a blink, only the actions missed come.
//! - **The outbox** ([`Outbox`]) holds every intent (a seen mark, a name) under the id it was first
//!   sent with, sent again under the same id after a reconnect or a relaunch until the worker
//!   answers, and the worker acts on an id once.
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
use slopty_proto::thread::{ContentRef, IntentId, TableState, ThreadId};

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
    /// The thread's state moved on.
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
    /// Each open thread's last review of each scope asked: a review tile's and the thread's
    /// own card of its last turn's changes ask different scopes of one thread at once.
    reviews: HashMap<ThreadId, Vec<Arc<Review>>>,
    /// Reviews asked for and not come yet, by thread and scope: asked again on the next link,
    /// since one asked into a link that was going is lost with it.
    reviews_asked: Vec<(ThreadId, ReviewScope)>,
    /// Expansions asked for and not come yet.
    asked: HashSet<ContentRef>,
    /// Pages of older turns asked for and not come yet, by thread: the first turn held when
    /// each was asked.
    paging: HashMap<ThreadId, slopty_proto::thread::TurnId>,
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

    /// `thread`'s title: its state's when followed here and titled, else its table row's.
    #[must_use]
    pub fn title(&self, thread: ThreadId) -> Option<&str> {
        let followed = self.mirror(thread).and_then(Mirror::state).map(|s| s.meta.title.as_str());
        followed
            .filter(|t| !t.is_empty())
            .or_else(|| self.table.rows.get(&thread).map(|r| r.title.as_str()))
            .filter(|t| !t.is_empty())
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
        out.extend(self.mirrors.iter().map(|(thread, mirror)| follow(*thread, mirror)));
        out.extend(self.outbox.all().iter().map(intent_msg));
        out.extend(self.reviews_asked.iter().map(|(thread, scope)| review_msg(*thread, scope)));
        out
    }

    /// The link went: every mirror shows what it last knew.
    pub fn disconnected(&mut self) {
        self.linked = false;
        // What was asked and not answered is lost with the link.
        self.asked.clear();
        self.paging.clear();
        for mirror in self.mirrors.values_mut() {
            mirror.lost();
        }
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
        self.reviews_asked.retain(|(t, _)| *t != thread);
        self.paging.remove(&thread);
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
            self.reviews_asked.retain(|(t, scope)| !(*t == thread && *scope == review.scope));
            let held = self.reviews.entry(thread).or_default();
            held.retain(|r| r.scope != review.scope);
            held.push(Arc::from(review));
            return (Changed::Review, Vec::new());
        }
        if matches!(frame, ThreadFrame::Page(_)) {
            self.paging.remove(&thread);
        }
        let Some(mirror) = self.mirrors.get_mut(&thread) else {
            return (Changed::Nothing, Vec::new());
        };
        match mirror.take(frame) {
            Took::Moved => (Changed::Thread, Vec::new()),
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

    /// The worker's answer to an intent: the intent, when it was one of this client's, which is
    /// on its way no longer.
    pub fn done(&mut self, done: &IntentDone) -> Option<Sent> {
        let sent = self.outbox.answered(done.id)?;
        self.outbox_changed = true;
        Some(sent)
    }

    /// Ask `thread` to do `intent`: its id, and what to send now (nothing while the link is
    /// down; it goes once the link is back).
    pub fn intent(&mut self, thread: ThreadId, intent: Intent) -> (IntentId, Option<ClientMsg>) {
        let id = IntentId::new();
        (id, self.intent_as(id, thread, intent))
    }

    /// Ask `thread` to do `intent` under `id`, the one its origin gave it (a message's, as the
    /// interface named it): what to send now, as [`Self::intent`].
    pub fn intent_as(
        &mut self,
        id: IntentId,
        thread: ThreadId,
        intent: Intent,
    ) -> Option<ClientMsg> {
        let sent = Sent { id, thread, intent };
        let msg = self.linked.then(|| intent_msg(&sent));
        self.outbox.push(sent);
        self.outbox_changed = true;
        msg
    }

    /// Whether this client already asked anything under `id`, which a thread acts on once.
    #[must_use]
    pub fn asked(&self, id: IntentId) -> bool {
        self.outbox.all().iter().any(|sent| sent.id == id)
    }

    /// What to send to see what `thread` changed over `scope`; the answer comes as
    /// [`Changed::Review`]. Until it comes it is asked again on every link: one asked while the
    /// link is down goes once it is back, and one lost with a link that went goes again.
    pub fn ask_review(&mut self, thread: ThreadId, scope: ReviewScope) -> Option<ClientMsg> {
        if !self.mirrors.contains_key(&thread) {
            return None;
        }
        let msg = self.linked.then(|| review_msg(thread, &scope));
        if !self.reviews_asked.iter().any(|(t, s)| *t == thread && *s == scope) {
            self.reviews_asked.push((thread, scope));
        }
        msg
    }

    /// The last review of `thread` over `scope` that came.
    #[must_use]
    pub fn review(&self, thread: ThreadId, scope: &ReviewScope) -> Option<&Arc<Review>> {
        self.reviews.get(&thread)?.iter().find(|r| r.scope == *scope)
    }

    /// The whole of `content`, when it came.
    pub fn expanded(&mut self, content: &ContentRef) -> Option<Arc<Expanded>> {
        self.blobs.get(content)
    }

    /// The whole of `content`, when it came, for a reader that only looks.
    #[must_use]
    pub fn whole(&self, content: &ContentRef) -> Option<&Arc<Expanded>> {
        self.blobs.peek(content)
    }

    /// Whether a table frame has come since the client started: the table is the worker's
    /// word, not an empty start.
    #[must_use]
    pub const fn heard_table(&self) -> bool {
        self.heard_table
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

    /// Ask for the turns of `thread` before the first one held: nothing when it holds them
    /// all, the link is down, or the same page is on its way.
    pub fn page(&mut self, thread: ThreadId, turns: u32) -> Option<ClientMsg> {
        let state = self.mirrors.get(&thread)?.state()?;
        let before = state.turns.first()?.id;
        if !state.older || !self.linked || self.paging.get(&thread) == Some(&before) {
            return None;
        }
        self.paging.insert(thread, before);
        Some(ClientMsg::Thread(ThreadRequest::Page { thread, before, turns }))
    }

    /// Whether `thread` holds turns before its first one that it has not fetched yet.
    #[must_use]
    pub fn older(&self, thread: ThreadId) -> bool {
        self.mirrors.get(&thread).and_then(Mirror::state).is_some_and(|state| state.older)
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

fn review_msg(thread: ThreadId, scope: &ReviewScope) -> ClientMsg {
    ClientMsg::Thread(ThreadRequest::Review { thread, scope: scope.clone() })
}

fn cached(mirror: &Mirror) -> Option<Cached> {
    Some(Cached { cursor: mirror.cursor()?, state: mirror.state()?.clone() })
}

#[cfg(test)]
mod tests;
