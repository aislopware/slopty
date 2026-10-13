//! Words on their way to an agent (`docs/decisions/projects.md`, "A parent hears through its
//! hooks").
//!
//! An agent hears of the agents it started, and of what another agent tells it, through its
//! own hooks: what is for it waits here by its thread, and goes:
//!
//! - what the server says of a child that came to rest once that has settled for [`REST_SETTLE`],
//!   so the parent reads the child's last words and not a flurry;
//! - a child waiting on the person once the wait has lasted [`WAIT_SETTLE`]: a permission the
//!   person answers at once wakes nobody;
//! - a child whose program ended, and words told it ([`Deliveries::words`]), at once.
//!
//! The server's later word about a child replaces its earlier one still waiting, and the
//! child going back to work takes it back ([`Deliveries::moved_on`]); a parent that read the
//! child itself (`agent_wait`, `agent_read`) is not told again ([`Deliveries::seen`]).
//!
//! A batch goes to the agent's live seat, whose worker hands it over through its hooks (or as
//! a message to a thread with no terminal) and says so ([`Deliveries::acked`]). Until then it
//! stays outstanding: sent again when the worker registers again, folded into the next batch
//! when more falls due or after [`RESEND`] when the link could not take it
//! ([`Deliveries::unsent`]), and put back to wait when its seat closes. Nothing is ever typed
//! into a terminal.
//!
//! What waits and what is outstanding outlives the server: the store keeps it
//! ([`Deliveries::kept`], [`Deliveries::adopt`]), and each change is said
//! ([`Deliveries::changes`]). A batch's number is never used twice, across restarts too, nor
//! is a word's ([`ReportBlock::id`]), so a worker leaves out a word its agent read already.
//!
//! Pure but for that word of a change: the hub keeps it under its lock and says what time it is.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::server::{ReportBlock, Reports};
use slopty_proto::thread::ThreadId;
use tokio::sync::watch;
use tokio::time::Instant;

/// How long a child's rest settles before its parent hears of it.
pub(crate) const REST_SETTLE: Duration = Duration::from_secs(10);
/// How long a child waits on the person before its parent hears so.
pub(crate) const WAIT_SETTLE: Duration = Duration::from_secs(30);
/// How long a batch the link could not take waits before it goes again, with whatever came
/// since: a link full now has drained by then.
pub(crate) const RESEND: Duration = Duration::from_secs(1);
/// The longest batch, in bytes: Claude Code takes up to 10 000 characters of a hook's
/// context.
pub(crate) const CONTEXT_MAX: usize = 9_000;

/// What the server says of a child, which decides when it goes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub(crate) enum Kind {
    /// It came to rest: once that has settled.
    Rest,
    /// It waits on the person: once the wait has lasted.
    Needs,
    /// Its program ended: at once.
    Ended,
}

/// Who wrote a word waiting for an agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum By {
    /// The server, of the child `about`.
    Outcome(Kind),
    /// Another agent, named as it reads.
    Agent(String),
    /// The person.
    Person,
}

/// A word waiting for an agent.
#[derive(Clone, Debug)]
struct Item {
    /// Its number, the block's id in every batch it rides in.
    id: u64,
    /// The child it is about, or the agent that said it.
    about: Option<ThreadId>,
    by: By,
    note: String,
    at: Instant,
}

impl Item {
    /// When it falls due.
    fn due(&self) -> Instant {
        let after = |wait: Duration| self.at.checked_add(wait).unwrap_or(self.at);
        match self.by {
            By::Outcome(Kind::Rest) => after(REST_SETTLE),
            By::Outcome(Kind::Needs) => after(WAIT_SETTLE),
            By::Outcome(Kind::Ended) | By::Agent(_) | By::Person => self.at,
        }
    }

    /// Whether it is the server's word about `child`.
    fn outcome_about(&self, child: ThreadId) -> bool {
        matches!(self.by, By::Outcome(_)) && self.about == Some(child)
    }
}

/// A batch sent and not yet handed over.
#[derive(Debug)]
struct Outstanding {
    batch: u64,
    term: TermRef,
    items: Vec<Item>,
    /// The link could not take it: when it goes again.
    resend: Option<Instant>,
}

#[derive(Debug, Default)]
struct Queue {
    waiting: Vec<Item>,
    outstanding: Option<Outstanding>,
    /// A word came since the last batch went: what waits may go before that one is read.
    fresh: bool,
    /// Its agent had no live seat when it fell due: it waits for one ([`Deliveries::unpark`]).
    parked: bool,
}

impl Queue {
    /// When what waits falls due. While a batch is out, only a word that came since makes
    /// another; a batch the link could not take goes again when it is due to.
    fn due(&self) -> Option<Instant> {
        if self.parked {
            return None;
        }
        let resend = self.outstanding.as_ref().and_then(|o| o.resend);
        if self.outstanding.is_some() && !self.fresh {
            return resend;
        }
        self.waiting.iter().map(Item::due).chain(resend).min()
    }
}

/// A batch to push to a worker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Batch {
    /// The agent it is for.
    pub to: ThreadId,
    /// The seat it goes to.
    pub term: TermRef,
    /// Its number.
    pub number: u64,
    /// The words the agent reads.
    pub reports: Reports,
}

/// Every agent's words.
#[derive(Debug)]
pub(crate) struct Deliveries {
    queues: BTreeMap<ThreadId, Queue>,
    next_batch: u64,
    /// The last word's number ([`Item::id`]).
    next_item: u64,
    /// Counts the changes the store keeps.
    changes: watch::Sender<u64>,
}

impl Default for Deliveries {
    fn default() -> Self {
        Self {
            queues: BTreeMap::new(),
            next_batch: 0,
            next_item: 0,
            changes: watch::Sender::new(0),
        }
    }
}

impl Deliveries {
    /// What the server says of `child` for its parent `to`: `note`, paced as `kind`. It
    /// replaces what it said of the child before still waiting.
    pub(crate) fn outcome(
        &mut self,
        to: ThreadId,
        child: ThreadId,
        kind: Kind,
        note: &str,
        at: Instant,
    ) {
        let id = self.next_id();
        let item = Item { id, about: Some(child), by: By::Outcome(kind), note: plain(note), at };
        let queue = self.queues.entry(to).or_default();
        queue.waiting.retain(|i| !i.outcome_about(child));
        Self::push(queue, item);
        self.changed();
    }

    /// Words for `to` from `from` (another agent, by its name as `to` reads it, and its
    /// thread), or from the person when `from` is absent: they go at once, after what was said
    /// before, and every one is kept.
    pub(crate) fn words(
        &mut self,
        to: ThreadId,
        from: Option<(String, ThreadId)>,
        words: &str,
        at: Instant,
    ) {
        let id = self.next_id();
        let (by, about) = match from {
            Some((name, thread)) => (By::Agent(plain(&name)), Some(thread)),
            None => (By::Person, None),
        };
        let item = Item { id, about, by, note: plain(words), at };
        let queue = self.queues.entry(to).or_default();
        // Words are tried at once even where the rest waits parked.
        queue.parked = false;
        Self::push(queue, item);
        self.changed();
    }

    fn push(queue: &mut Queue, item: Item) {
        queue.waiting.push(item);
        queue.fresh = true;
    }

    /// `child` went back to work: what the server was to say of it, and has not sent, no
    /// longer holds.
    pub(crate) fn moved_on(&mut self, child: ThreadId) {
        let mut any = false;
        for queue in self.queues.values_mut() {
            let before = queue.waiting.len();
            queue.waiting.retain(|i| !i.outcome_about(child));
            any |= queue.waiting.len() != before;
        }
        if any {
            self.changed();
        }
        self.prune();
    }

    /// `to` read where `child` stands itself: what the server was to say of it, and has not
    /// sent, is no news to it.
    pub(crate) fn seen(&mut self, to: ThreadId, child: ThreadId) {
        if let Some(queue) = self.queues.get_mut(&to) {
            let before = queue.waiting.len();
            queue.waiting.retain(|i| !i.outcome_about(child));
            if queue.waiting.len() != before {
                self.changed();
            }
        }
        self.prune();
    }

    /// Read again what the server says of each child, as it waits: `words` gives the note for
    /// the child and kind now, or keeps it when it gives none.
    pub(crate) fn reword(&mut self, mut words: impl FnMut(ThreadId, Kind) -> Option<String>) {
        let mut any = false;
        for queue in self.queues.values_mut() {
            for item in &mut queue.waiting {
                let (By::Outcome(kind), Some(child)) = (&item.by, item.about) else { continue };
                if let Some(note) = words(child, *kind) {
                    let note = plain(&note);
                    any |= item.note != note;
                    item.note = note;
                }
            }
        }
        if any {
            self.changed();
        }
    }

    /// A new word's number ([`Item::id`]).
    const fn next_id(&mut self) -> u64 {
        self.next_item = self.next_item.wrapping_add(1);
        self.next_item
    }

    /// When the next batch falls due, if anything waits.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.queues.values().filter_map(Queue::due).min()
    }

    /// The batches due by `now`, each for its agent's live seat as `seat_of` names it. What
    /// waits for an agent with no live seat stays, parked until one comes ([`Self::unpark`]).
    /// A batch holds what its agent had outstanding too, and replaces it.
    pub(crate) fn take(
        &mut self,
        now: Instant,
        mut seat_of: impl FnMut(ThreadId) -> Option<TermRef>,
    ) -> Vec<Batch> {
        let mut out = Vec::new();
        for (to, queue) in &mut self.queues {
            if queue.due().is_none_or(|due| due > now) {
                continue;
            }
            let Some(term) = seat_of(*to) else {
                queue.parked = true;
                continue;
            };
            let mut items = queue.outstanding.take().map(|o| o.items).unwrap_or_default();
            items.append(&mut queue.waiting);
            let (reports, items, left) = context(items);
            // What did not fit waits for the next batch, due as it was.
            queue.waiting = left;
            queue.fresh = false;
            self.next_batch = self.next_batch.wrapping_add(1);
            let batch = self.next_batch;
            queue.outstanding = Some(Outstanding { batch, term, items, resend: None });
            out.push(Batch { to: *to, term, number: batch, reports });
        }
        self.prune();
        if !out.is_empty() {
            self.changed();
        }
        out
    }

    /// The link to `to`'s worker could not take batch `batch` now: it goes again [`RESEND`]
    /// after `now`, as the next batch with whatever came since, unless something else makes
    /// one first.
    pub(crate) fn unsent(&mut self, to: ThreadId, batch: u64, now: Instant) {
        let outstanding = self.queues.get_mut(&to).and_then(|q| q.outstanding.as_mut());
        if let Some(o) = outstanding.filter(|o| o.batch == batch) {
            o.resend = Some(now.checked_add(RESEND).unwrap_or(now));
        }
    }

    /// A seat may have come for an agent whose words wait for one: they fall due again.
    /// Whether any waited.
    pub(crate) fn unpark(&mut self) -> bool {
        let mut any = false;
        for queue in self.queues.values_mut() {
            any |= std::mem::take(&mut queue.parked);
        }
        any
    }

    /// `term` handed batch `batch` to its agent: the agent it was for, unless it was replaced
    /// since.
    pub(crate) fn acked(&mut self, term: TermRef, batch: u64) -> Option<ThreadId> {
        let (to, queue) = self.queues.iter_mut().find(|(_, q)| {
            q.outstanding.as_ref().is_some_and(|o| o.term == term && o.batch == batch)
        })?;
        queue.outstanding = None;
        let to = *to;
        // Held back only while the batch was out.
        queue.fresh = true;
        self.prune();
        self.changed();
        Some(to)
    }

    /// The batches outstanding on seats of `worker`, to send again after it registers.
    pub(crate) fn outstanding_on(&self, worker: WorkerId) -> Vec<Batch> {
        self.queues
            .iter()
            .filter_map(|(to, q)| {
                let o = q.outstanding.as_ref().filter(|o| o.term.worker == worker)?;
                Some(Batch {
                    to: *to,
                    term: o.term,
                    number: o.batch,
                    reports: context(o.items.clone()).0,
                })
            })
            .collect()
    }

    /// `term` closed: what it was sent and never handed over waits for the agent's next seat,
    /// due at once, since no agent read it.
    pub(crate) fn closed(&mut self, term: TermRef) {
        let mut any = false;
        for queue in self.queues.values_mut() {
            if queue.outstanding.as_ref().is_some_and(|o| o.term == term)
                && let Some(o) = queue.outstanding.take()
            {
                let mut items = o.items;
                items.append(&mut queue.waiting);
                queue.waiting = items;
                any = true;
            }
        }
        if any {
            self.changed();
        }
    }

    /// The seats of `worker` that batches are outstanding on and that it no longer has (`open`
    /// are those it has): they closed while the server was away.
    pub(crate) fn gone_on(&self, worker: WorkerId, open: &[SessionId]) -> Vec<TermRef> {
        self.queues
            .values()
            .filter_map(|q| q.outstanding.as_ref().map(|o| o.term))
            .filter(|t| t.worker == worker && !open.contains(&t.session))
            .collect()
    }

    /// Says each change the store keeps: what waits, what is outstanding, a batch's number.
    pub(crate) fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// What the store keeps, as of `now`, which is `wall` on the clock: when each word came
    /// is kept as wall time, since an [`Instant`] means nothing to the next process.
    pub(crate) fn kept(&self, now: Instant, wall: WallMs) -> Kept {
        let item = |i: &Item| {
            let ago =
                u64::try_from(now.saturating_duration_since(i.at).as_millis()).unwrap_or(u64::MAX);
            KeptItem {
                id: i.id,
                about: i.about,
                by: i.by.clone(),
                note: i.note.clone(),
                at: WallMs::from_millis(wall.as_millis().saturating_sub(ago)),
            }
        };
        let queues = self
            .queues
            .iter()
            .map(|(to, q)| KeptQueue {
                to: *to,
                waiting: q.waiting.iter().map(item).collect(),
                outstanding: q.outstanding.as_ref().map(|o| KeptBatch {
                    batch: o.batch,
                    term: o.term,
                    items: o.items.iter().map(item).collect(),
                }),
                fresh: q.fresh,
            })
            .collect();
        Kept { next_batch: self.next_batch, next_item: self.next_item, queues }
    }

    /// Take up what the store kept, as of `now`, which is `wall` on the clock, in place of
    /// what is here. Batch and word numbers go on past the kept ones and past `wall` in
    /// milliseconds: a store lost or set aside never makes a number a worker saw before.
    pub(crate) fn adopt(&mut self, kept: Kept, now: Instant, wall: WallMs) {
        let item = |i: KeptItem| {
            let ago = Duration::from_millis(wall.as_millis().saturating_sub(i.at.as_millis()));
            Item {
                id: i.id,
                about: i.about,
                by: i.by,
                note: i.note,
                at: now.checked_sub(ago).unwrap_or(now),
            }
        };
        self.queues = kept
            .queues
            .into_iter()
            .map(|q| {
                let queue = Queue {
                    waiting: q.waiting.into_iter().map(item).collect(),
                    outstanding: q.outstanding.map(|o| Outstanding {
                        batch: o.batch,
                        term: o.term,
                        items: o.items.into_iter().map(item).collect(),
                        resend: None,
                    }),
                    fresh: q.fresh,
                    parked: false,
                };
                (q.to, queue)
            })
            .collect();
        self.next_batch = kept.next_batch.max(wall.as_millis());
        self.next_item = kept.next_item.max(wall.as_millis());
        self.prune();
        self.changed();
    }

    /// Drop queues with nothing in them.
    fn prune(&mut self) {
        self.queues.retain(|_, q| !q.waiting.is_empty() || q.outstanding.is_some());
    }

    /// How many words wait or are outstanding, in all.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.queues
            .values()
            .map(|q| {
                q.waiting.len().saturating_add(q.outstanding.as_ref().map_or(0, |o| o.items.len()))
            })
            .sum()
    }
}

/// What waits for the agents and what is outstanding, as the store keeps it
/// ([`crate::store::DELIVERIES_FILE`]).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Kept {
    next_batch: u64,
    next_item: u64,
    queues: Vec<KeptQueue>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct KeptQueue {
    to: ThreadId,
    waiting: Vec<KeptItem>,
    outstanding: Option<KeptBatch>,
    fresh: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct KeptBatch {
    batch: u64,
    term: TermRef,
    items: Vec<KeptItem>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct KeptItem {
    id: u64,
    about: Option<ThreadId>,
    by: By,
    note: String,
    at: WallMs,
}

/// The batch as the agent reads it: one block per word, the person's first, then other
/// agents', then the server's, as many as fit [`CONTEXT_MAX`]. The words it holds, and those
/// left for the next batch, come back with it.
fn context(mut items: Vec<Item>) -> (Reports, Vec<Item>, Vec<Item>) {
    let rank = |i: &Item| match i.by {
        By::Person => 0_u8,
        By::Agent(_) => 1,
        By::Outcome(_) => 2,
    };
    items.sort_by_key(|i| (rank(i), i.at));
    let head = "<slopty-reports>";
    let tail = "</slopty-reports>";
    let room = CONTEXT_MAX.saturating_sub(head.len()).saturating_sub(tail.len()).saturating_sub(64);
    let mut blocks: Vec<ReportBlock> = Vec::new();
    let mut used = 0_usize;
    let mut taken = Vec::new();
    let mut left = Vec::new();
    for item in items {
        let block = block(&item);
        // Always one, cut if it must be, so a batch is never empty.
        let block = if taken.is_empty() && block.len() > room { cut(&block, room) } else { block };
        if !left.is_empty() || used.saturating_add(block.len()) > room {
            left.push(item);
            continue;
        }
        used = used.saturating_add(block.len()).saturating_add(1);
        blocks.push(ReportBlock { id: item.id, text: block });
        taken.push(item);
    }
    let close = if left.is_empty() {
        tail.to_owned()
    } else {
        format!("({} more follow once these are read)\n{tail}", left.len())
    };
    (Reports { open: head.to_owned(), blocks, close }, taken, left)
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// One word as the agent reads it.
fn block(item: &Item) -> String {
    let said = match &item.by {
        By::Person => "The person says:".to_owned(),
        By::Agent(name) => {
            let thread = item.about.map(|t| format!(" ({t})")).unwrap_or_default();
            format!("Agent {name}{thread} says (an agent, not the person):")
        }
        By::Outcome(_) => return item.note.trim().to_owned(),
    };
    let mut lines = vec![said];
    lines.extend(item.note.trim().lines().map(|line| format!("  {line}")));
    lines.join("\n")
}

/// `text` with the block's own tag name, in any case, spelled apart: an agent's words that
/// read as `</slopty-reports>` would end the block and pass as the server's. Every word an
/// agent wrote goes through it.
pub(crate) fn plain(text: &str) -> String {
    const TAG: &str = "slopty-reports";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.to_ascii_lowercase().find(TAG) {
        let (before, after) = rest.split_at(at);
        out.push_str(before);
        out.push_str("slopty reports");
        rest = after.get(TAG.len()..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests;
