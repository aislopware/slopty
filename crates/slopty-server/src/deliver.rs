//! Reports on their way to the agent they are for (`docs/decisions/projects.md`, "Reports go
//! up through hooks").
//!
//! A task's agent reports its finished work to the project's orchestrator. What goes to an
//! agent waits here per node (the orchestrator, or a task for the person's and the
//! orchestrator's words to it), and goes when its [`Kind`] says:
//!
//! - a finish ([`Kind::Done`]) once it has settled for [`DONE_SETTLE`], so the agent hears the last
//!   word and not a flurry: a later report of the task replaces it;
//! - a need ([`Kind::NeedsInput`]) or a block ([`Kind::Stuck`]) at once.
//!
//! A task's later report replaces its earlier one still waiting, so what waits for a node is
//! bounded by its tasks. Every report is on the timeline at once whatever waits here.
//!
//! What the server says of a task's agent that did not report ([`Deliveries::outcome`]: it came
//! to rest, waits on the person, or exited) is paced as the report it stands for, a wait on the
//! person once it has lasted [`WAIT_SETTLE`]. The agent's own report replaces it, and so does
//! its next outcome; the agent going back to work takes it back ([`Deliveries::moved_on`]).
//!
//! A batch goes to the node's live terminal, whose worker hands it to the agent through its
//! hooks and says so ([`Deliveries::acked`]). Until then it stays outstanding: sent again when
//! the worker registers again, folded into the next batch when more falls due or after
//! [`RESEND`] when the link could not take it ([`Deliveries::unsent`]), and put back to wait for
//! the node's next terminal when this one closes. Nothing is ever typed into a terminal.
//!
//! What waits and what is outstanding outlives the server: the store keeps it
//! ([`Deliveries::kept`], [`Deliveries::adopt`]), and each change is said
//! ([`Deliveries::changes`]). A batch's number is never used twice, across restarts too, so a
//! worker knows a batch it handed over already when it comes again; nor is a word's
//! ([`ReportBlock::id`]), which it keeps in every batch it rides in, so a worker leaves out a
//! word its agent read in a batch whose word of being read was lost.
//!
//! Pure but for that word of a change: the hub keeps it under its lock and says what time it is.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{ProjectId, Report, TaskId};
use slopty_proto::server::{ReportBlock, Reports};
use tokio::sync::watch;
use tokio::time::Instant;

/// How long a finish settles before it is delivered.
pub(crate) const DONE_SETTLE: Duration = Duration::from_mins(2);
/// How long a task's agent waits on the person before the orchestrator hears so: a
/// permission the person answers at once wakes nobody.
pub(crate) const WAIT_SETTLE: Duration = Duration::from_secs(30);
/// How long a batch the link could not take waits before it goes again, with whatever came
/// since: a link full now has drained by then.
pub(crate) const RESEND: Duration = Duration::from_secs(1);
/// The longest batch, in bytes: Claude Code takes up to 10 000 characters of a hook's
/// context.
pub(crate) const CONTEXT_MAX: usize = 9_000;

/// Where words go in a project: a task's agent, or the orchestrator when the task is absent.
pub(crate) type Node = (ProjectId, Option<TaskId>);

/// What a word waiting for a node says of its task, which decides when it goes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub(crate) enum Kind {
    /// The work is finished: it goes once it has settled.
    Done,
    /// An answer is wanted: at once, or for an agent waiting on the person once the wait
    /// lasted [`WAIT_SETTLE`].
    NeedsInput,
    /// It cannot go on: at once.
    Stuck,
}

/// Who wrote what waits for a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum By {
    /// A task's agent: its report.
    Agent,
    /// The server: standing instructions when the task is absent, or a notice about the task
    /// (its verifier failed, it merged), read as they are.
    Server,
    /// The person, to the node's own agent: their words, which go at once and are all kept.
    Person,
    /// The server, of what a task's agent came to when it did not report: its last words
    /// with it, read again until it goes ([`Deliveries::reword`]).
    Outcome,
    /// The orchestrator, to a task's own agent. Its words go at once, after the person's, and
    /// its latest replaces the one still unread.
    Orchestrator,
}

/// What waits for a node: a task's report, the server's own words, or the person's.
#[derive(Clone, Debug)]
struct Item {
    /// Its number, the block's id in every batch it rides in.
    id: u64,
    task: Option<TaskId>,
    kind: Kind,
    report: Report,
    at: Instant,
    by: By,
}

impl Item {
    /// When it falls due.
    fn due(&self) -> Instant {
        if matches!(self.by, By::Person | By::Orchestrator) {
            return self.at;
        }
        let after = |wait: Duration| self.at.checked_add(wait).unwrap_or(self.at);
        match self.kind {
            Kind::NeedsInput if self.by == By::Outcome => after(WAIT_SETTLE),
            Kind::NeedsInput | Kind::Stuck => self.at,
            Kind::Done => after(DONE_SETTLE),
        }
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
    /// A report came since the last batch went: what waits may go before that one is read.
    fresh: bool,
    /// Its node had no live terminal when it fell due: it waits for one ([`Deliveries::unpark`]).
    parked: bool,
}

impl Queue {
    /// When what waits falls due. While a batch is out, only a report that came since makes
    /// another: what did not fit it waits for it to be read.
    /// A batch the link could not take goes again when it is due to.
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
    /// The node it is for.
    pub node: Node,
    /// The terminal it goes to.
    pub term: TermRef,
    /// Its number.
    pub number: u64,
    /// The words the agent reads.
    pub reports: Reports,
    /// How many tasks' reports it holds; the server's own words beside them are not counted.
    pub count: u16,
}

/// How many of `items` are tasks' reports, not the server's own words.
fn reports_in(items: &[Item]) -> u16 {
    u16::try_from(items.iter().filter(|i| i.task.is_some()).count()).unwrap_or(u16::MAX)
}

/// Every node's reports.
#[derive(Debug)]
pub(crate) struct Deliveries {
    queues: BTreeMap<Node, Queue>,
    next_batch: u64,
    /// The last word's number ([`Item::id`]).
    next_item: u64,
    /// What the server last sent each node of each task's agent, by its kind and words: the
    /// same again says nothing new and is not sent.
    told: HashMap<(Node, TaskId), (Kind, u64)>,
    /// Counts the changes the store keeps.
    changes: watch::Sender<u64>,
}

impl Default for Deliveries {
    fn default() -> Self {
        Self {
            queues: BTreeMap::new(),
            next_batch: 0,
            next_item: 0,
            told: HashMap::new(),
            changes: watch::Sender::new(0),
        }
    }
}

/// An outcome's kind and words, to know it again.
fn fingerprint(item: &Item) -> (Kind, u64) {
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&item.report.note, &mut hasher);
    (item.kind, std::hash::Hasher::finish(&hasher))
}

impl Deliveries {
    /// `task`'s report of its finished work for `node`: it replaces the task's report still
    /// waiting.
    pub(crate) fn add(&mut self, node: Node, task: Option<TaskId>, report: Report, at: Instant) {
        let id = self.next_id();
        self.push(node, Item { id, task, kind: Kind::Done, report, at, by: By::Agent });
    }

    /// The server's standing instructions for `node`'s agent (its role): they go at once, and
    /// replace those still waiting.
    pub(crate) fn instructions(&mut self, node: Node, note: &str, at: Instant) {
        let (kind, report) = (Kind::NeedsInput, words(note));
        let id = self.next_id();
        self.push(node, Item { id, task: None, kind, report, at, by: By::Server });
    }

    /// The server's own `note` about `task`, for `node`, paced as a report of `kind`: it
    /// replaces the server's earlier notice about the task still waiting, never the task's own
    /// reports, and counts as a report delivered.
    pub(crate) fn notice(&mut self, node: Node, task: TaskId, kind: Kind, note: &str, at: Instant) {
        // It quotes what others wrote (a verifier's output, git's words), so it is kept from
        // closing its block as an agent's words are.
        let report = words(note);
        let id = self.next_id();
        self.push(node, Item { id, task: Some(task), kind, report, at, by: By::Server });
    }

    /// What the server says of `task`'s agent, which did not report, for `node`: `note`, paced
    /// as a report of `kind`. It replaces what it said of the agent before still waiting, and
    /// the agent's own report replaces it.
    pub(crate) fn outcome(
        &mut self,
        node: Node,
        task: TaskId,
        kind: Kind,
        note: &str,
        at: Instant,
    ) {
        let report = words(note);
        let id = self.next_id();
        self.push(node, Item { id, task: Some(task), kind, report, at, by: By::Outcome });
    }

    /// `task`'s agent went back to work: what the server was to say of it for `node`, and has
    /// not sent, no longer holds.
    pub(crate) fn moved_on(&mut self, node: &Node, task: TaskId) {
        if let Some(queue) = self.queues.get_mut(node) {
            let before = queue.waiting.len();
            queue.waiting.retain(|i| i.by != By::Outcome || i.task != Some(task));
            if queue.waiting.len() != before {
                self.changed();
            }
        }
        self.prune();
    }

    /// Read again what the server says of each agent that did not report, as it waits:
    /// `words` gives the note for its node, task and kind now, or keeps it when it gives none.
    pub(crate) fn reword(&mut self, mut words: impl FnMut(&Node, TaskId, Kind) -> Option<String>) {
        let mut any = false;
        for (node, queue) in &mut self.queues {
            for item in queue.waiting.iter_mut().filter(|i| i.by == By::Outcome) {
                let kind = item.kind;
                if let Some(note) = item.task.and_then(|task| words(node, task, kind)) {
                    let note = plain(&note);
                    any |= item.report.note != note;
                    item.report.note = note;
                }
            }
        }
        if any {
            self.changed();
        }
    }

    /// The person's `words` to the agent of `task` itself, or to the orchestrator when it is
    /// absent: they go at once, after what the person said before that has not been read, and
    /// stand beside the server's notices.
    pub(crate) fn person(
        &mut self,
        project: ProjectId,
        task: Option<TaskId>,
        words: &str,
        at: Instant,
    ) {
        let (kind, report) = (Kind::NeedsInput, self::words(words));
        let id = self.next_id();
        self.push((project, task), Item { id, task, kind, report, at, by: By::Person });
    }

    /// The orchestrator's words to the agent of `node`'s task: they go at once, after the
    /// person's words and never in their place, and replace its own earlier words still unread.
    pub(crate) fn orchestrator(&mut self, node: Node, words: &str, at: Instant) {
        let (kind, report, task) = (Kind::NeedsInput, self::words(words), node.1);
        let id = self.next_id();
        self.push(node, Item { id, task, kind, report, at, by: By::Orchestrator });
    }

    /// A new word's number ([`Item::id`]).
    const fn next_id(&mut self) -> u64 {
        self.next_item = self.next_item.wrapping_add(1);
        self.next_item
    }

    fn push(&mut self, node: Node, item: Item) {
        let task = item.task;
        let queue = self.queues.entry(node).or_default();
        queue.waiting.retain(|i| {
            // Every word the person says is kept: a second message is not a newer first.
            let replaced = match item.by {
                By::Person => false,
                By::Server | By::Outcome => i.by == item.by,
                By::Orchestrator => i.by == By::Orchestrator,
                // The agent's own word stands for what the server would say of it.
                By::Agent => i.by == By::Agent || (i.by == By::Outcome && task.is_some()),
            };
            i.task != task || !replaced
        });
        // The person's words are tried at once even where the rest waits parked: a hold
        // ([`Self::take`]) lets them through.
        queue.parked &= item.by != By::Person;
        queue.waiting.push(item);
        queue.fresh = true;
        self.changed();
    }

    /// When the next batch falls due, if anything waits.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.queues.values().filter_map(Queue::due).min()
    }

    /// The batches due by `now`, each for its node's live terminal as `term_of` names it. What
    /// waits for a node with no live terminal stays, parked until one comes
    /// ([`Self::unpark`]). A batch holds what its node had outstanding too, and replaces it.
    pub(crate) fn take(
        &mut self,
        now: Instant,
        mut term_of: impl FnMut(&Node) -> Option<TermRef>,
    ) -> Vec<Batch> {
        let mut out = Vec::new();
        for (node, queue) in &mut self.queues {
            if queue.due().is_none_or(|due| due > now) {
                continue;
            }
            // An outcome that says again what was sent of the agent last is dropped as it
            // falls due, after its words were read for the last time.
            let told = &self.told;
            queue.waiting.retain(|i| {
                i.by != By::Outcome
                    || i.task.is_none_or(|t| told.get(&(node.clone(), t)) != Some(&fingerprint(i)))
            });
            if queue.due().is_none_or(|due| due > now) {
                continue;
            }
            let Some(term) = term_of(node) else {
                queue.parked = true;
                continue;
            };
            let mut items = queue.outstanding.take().map(|o| o.items).unwrap_or_default();
            items.append(&mut queue.waiting);
            let (reports, items, left) = context(&node.0, items);
            // What did not fit waits for the next batch, due as it was.
            queue.waiting = left;
            queue.fresh = false;
            for item in items.iter().filter(|i| i.by == By::Outcome) {
                if let Some(task) = item.task {
                    self.told.insert((node.clone(), task), fingerprint(item));
                }
            }
            self.next_batch = self.next_batch.wrapping_add(1);
            let batch = self.next_batch;
            let count = reports_in(&items);
            queue.outstanding = Some(Outstanding { batch, term, items, resend: None });
            out.push(Batch { node: node.clone(), term, number: batch, reports, count });
        }
        self.prune();
        if !out.is_empty() {
            self.changed();
        }
        out
    }

    /// The link to `node`'s worker could not take batch `batch` now: it goes again [`RESEND`]
    /// after `now`, as the next batch with whatever came since, unless something else makes
    /// one first.
    pub(crate) fn unsent(&mut self, node: &Node, batch: u64, now: Instant) {
        let outstanding = self.queues.get_mut(node).and_then(|q| q.outstanding.as_mut());
        if let Some(o) = outstanding.filter(|o| o.batch == batch) {
            o.resend = Some(now.checked_add(RESEND).unwrap_or(now));
        }
    }

    /// A terminal may have come for a node whose reports wait for one: they fall due again.
    /// Whether any waited.
    pub(crate) fn unpark(&mut self) -> bool {
        let mut any = false;
        for queue in self.queues.values_mut() {
            any |= std::mem::take(&mut queue.parked);
        }
        any
    }

    /// `term` handed batch `batch` to its agent: the node it was for and how many tasks'
    /// reports it held ([`Batch::reports`]), unless it was replaced since.
    pub(crate) fn acked(&mut self, term: TermRef, batch: u64) -> Option<(Node, u16)> {
        let (node, queue) = self.queues.iter_mut().find(|(_, q)| {
            q.outstanding.as_ref().is_some_and(|o| o.term == term && o.batch == batch)
        })?;
        let done = queue.outstanding.take()?;
        let node = node.clone();
        // Held back only while the batch was out.
        queue.fresh = true;
        self.prune();
        self.changed();
        Some((node, reports_in(&done.items)))
    }

    /// The batches outstanding on terminals of `worker`, to send again after it registers.
    pub(crate) fn outstanding_on(&self, worker: slopty_core::WorkerId) -> Vec<Batch> {
        self.queues
            .iter()
            .filter_map(|(node, q)| {
                let o = q.outstanding.as_ref().filter(|o| o.term.worker == worker)?;
                Some(Batch {
                    node: node.clone(),
                    term: o.term,
                    number: o.batch,
                    reports: context(&node.0, o.items.clone()).0,
                    count: reports_in(&o.items),
                })
            })
            .collect()
    }

    /// `term` closed: what it was sent and never handed over waits for the node's next
    /// terminal, due at once, since no agent read it.
    pub(crate) fn closed(&mut self, term: TermRef) {
        let mut any = false;
        for (node, queue) in &mut self.queues {
            if queue.outstanding.as_ref().is_some_and(|o| o.term == term)
                && let Some(o) = queue.outstanding.take()
            {
                // Never read, so not yet told: it is no repeat when it goes again.
                for item in o.items.iter().filter(|i| i.by == By::Outcome) {
                    if let Some(task) = item.task {
                        self.told.remove(&(node.clone(), task));
                    }
                }
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

    /// The terminals of `worker` that batches are outstanding on and that it no longer has
    /// (`open` are those it has): they closed while the server was away.
    pub(crate) fn gone_on(
        &self,
        worker: slopty_core::WorkerId,
        open: &[slopty_core::SessionId],
    ) -> Vec<TermRef> {
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
                task: i.task,
                kind: i.kind,
                report: i.report.clone(),
                at: WallMs::from_millis(wall.as_millis().saturating_sub(ago)),
                by: i.by,
            }
        };
        let queues = self
            .queues
            .iter()
            .map(|(node, q)| KeptQueue {
                node: node.clone(),
                waiting: q.waiting.iter().map(item).collect(),
                outstanding: q.outstanding.as_ref().map(|o| KeptBatch {
                    batch: o.batch,
                    term: o.term,
                    items: o.items.iter().map(item).collect(),
                }),
                fresh: q.fresh,
            })
            .collect();
        let told = self
            .told
            .iter()
            .map(|((node, task), (kind, words))| (node.clone(), *task, *kind, *words))
            .collect();
        Kept { next_batch: self.next_batch, next_item: self.next_item, queues, told }
    }

    /// Take up what the store kept, as of `now`, which is `wall` on the clock, in place of
    /// what is here. Batch and word numbers go on past the kept ones and past `wall` in
    /// milliseconds: a store lost or set aside never makes a number a worker saw before.
    pub(crate) fn adopt(&mut self, kept: Kept, now: Instant, wall: WallMs) {
        let item = |i: KeptItem| {
            let ago = Duration::from_millis(wall.as_millis().saturating_sub(i.at.as_millis()));
            Item {
                id: i.id,
                task: i.task,
                kind: i.kind,
                report: i.report,
                at: now.checked_sub(ago).unwrap_or(now),
                by: i.by,
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
                (q.node, queue)
            })
            .collect();
        self.told = kept
            .told
            .into_iter()
            .map(|(node, task, kind, words)| ((node, task), (kind, words)))
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

    /// How many reports wait or are outstanding, in all.
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
    told: Vec<(Node, TaskId, Kind, u64)>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct KeptQueue {
    node: Node,
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
    task: Option<TaskId>,
    kind: Kind,
    report: Report,
    at: WallMs,
    by: By,
}

/// `note` as a word of the server's or the person's, with no report's fields beside it.
fn words(note: &str) -> Report {
    Report { note: plain(note), artifacts: Vec::new(), branch: None, pr: None }
}

/// The batch as the agent reads it: one block per report, the person's words first, then the
/// server's own, as many as fit [`CONTEXT_MAX`]. The reports it holds, and those left for the
/// next batch, come back with it.
fn context(project: &ProjectId, mut items: Vec<Item>) -> (Reports, Vec<Item>, Vec<Item>) {
    items.sort_by_key(|i| (i.by != By::Person, i.task.is_some(), i.at));
    let head = format!("<slopty-reports project=\"{project}\">");
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
    (Reports { open: head, blocks, close }, taken, left)
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// One report as the agent reads it; the server's own words as they are. A report's words are
/// an agent's, so they never close the block they sit in ([`plain`]).
fn block(item: &Item) -> String {
    let r = &item.report;
    let said = match item.by {
        By::Person => Some("The person says:".to_owned()),
        By::Orchestrator => Some(
            "Your orchestrator says (an agent, not the person; it answers nothing the person is \
             asked):"
                .to_owned(),
        ),
        By::Agent | By::Server | By::Outcome => None,
    };
    if let Some(said) = said {
        let mut lines = vec![said];
        lines.extend(r.note.trim().lines().map(|line| format!("  {line}")));
        return lines.join("\n");
    }
    let Some(task) = item.task.filter(|_| item.by == By::Agent) else {
        return r.note.trim().to_owned();
    };
    let mut lines = vec![format!("task {task}: done")];
    lines.extend(r.note.trim().lines().map(|line| format!("  {}", plain(line))));
    lines.extend(r.branch.iter().map(|branch| format!("  branch: {}", plain(branch))));
    lines.extend(r.pr.iter().map(|pr| format!("  pull request: #{pr}")));
    lines.extend(r.artifacts.iter().map(|artifact| format!("  made: {}", plain(artifact))));
    lines.join("\n")
}

/// `text` with the block's own tag name, in any case, spelled apart: an agent's words that
/// read as `</slopty-reports>` would end the block and pass as the server's. Every word an agent
/// wrote goes through it, in the server's own words too (a project's title, its rules).
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
