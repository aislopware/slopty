//! Reports on their way to the agent they are for (`docs/decisions/projects.md`, "Reports go
//! up through hooks").
//!
//! A task's agent reports to the node that split its task off: its parent task's agent, or the
//! project's orchestrator. The reports wait here per node, and go when their kind says:
//!
//! - a need ([`ReportKind::NeedsInput`]) at once;
//! - a block ([`ReportKind::Stuck`]) at once, but at most once per task every [`STUCK_EVERY`];
//! - a finish ([`ReportKind::Done`]) once it has settled for [`DONE_SETTLE`], so the agent hears
//!   the last word and not a flurry: a later report of the task replaces it;
//! - a checkpoint with whatever goes next, or after [`CHECKPOINT_WAIT`].
//!
//! A batch goes to the node's live terminal, whose worker hands it to the agent through its
//! hooks and says so ([`Deliveries::acked`]). Until then it stays outstanding: sent again when
//! the worker registers again, folded into the next batch when more falls due, and put back to
//! wait for the node's next terminal when this one closes. Nothing is ever typed into a
//! terminal.
//!
//! Pure: the hub keeps it under its lock and says what time it is.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{ProjectId, Report, ReportKind, TaskId};
use tokio::time::Instant;

/// How long a finish settles before it is delivered.
pub(crate) const DONE_SETTLE: Duration = Duration::from_mins(2);
/// How long a checkpoint waits for something to ride with.
pub(crate) const CHECKPOINT_WAIT: Duration = Duration::from_hours(1);
/// How often one task's block may interrupt.
pub(crate) const STUCK_EVERY: Duration = Duration::from_mins(3);
/// The longest batch, in bytes: Claude Code takes up to 10 000 characters of a hook's
/// context.
pub(crate) const CONTEXT_MAX: usize = 9_000;

/// A node of a project's tree: a task, or the orchestrator's when the task is absent.
pub(crate) type Node = (ProjectId, Option<TaskId>);

/// What waits for a node: a task's report, or the server's own words (standing instructions)
/// when the task is absent.
#[derive(Clone, Debug)]
struct Item {
    task: Option<TaskId>,
    report: Report,
    at: Instant,
}

impl Item {
    /// When it falls due, given when its task's block last went.
    fn due(&self, stuck_last: Option<Instant>) -> Instant {
        let after = |wait: Duration| self.at.checked_add(wait).unwrap_or(self.at);
        match self.report.kind {
            ReportKind::NeedsInput => self.at,
            ReportKind::Stuck => {
                let next = stuck_last.and_then(|last| last.checked_add(STUCK_EVERY));
                next.map_or(self.at, |next| next.max(self.at))
            }
            ReportKind::Done => after(DONE_SETTLE),
            ReportKind::Checkpoint => after(CHECKPOINT_WAIT),
        }
    }
}

/// A batch sent and not yet handed over.
#[derive(Debug)]
struct Outstanding {
    batch: u64,
    term: TermRef,
    items: Vec<Item>,
}

#[derive(Debug, Default)]
struct Queue {
    waiting: Vec<Item>,
    outstanding: Option<Outstanding>,
    stuck_last: HashMap<TaskId, Instant>,
    /// A report came since the last batch went: what waits may go before that one is read.
    fresh: bool,
    /// Its node had no live terminal when it fell due: it waits for one ([`Deliveries::unpark`]).
    parked: bool,
}

impl Queue {
    /// When what waits falls due. While a batch is out, only a report that came since makes
    /// another: what did not fit it waits for it to be read.
    fn due(&self) -> Option<Instant> {
        if self.parked || (self.outstanding.is_some() && !self.fresh) {
            return None;
        }
        let stuck_last = |i: &Item| i.task.and_then(|t| self.stuck_last.get(&t).copied());
        self.waiting.iter().map(|i| i.due(stuck_last(i))).min()
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
    pub context: String,
    /// How many reports it holds.
    pub reports: u16,
}

/// Every node's reports.
#[derive(Debug, Default)]
pub(crate) struct Deliveries {
    queues: BTreeMap<Node, Queue>,
    next_batch: u64,
}

impl Deliveries {
    /// A report of `task` for `node`: it replaces the task's checkpoint and finish still
    /// waiting, and waits with its needs and blocks.
    pub(crate) fn add(&mut self, node: Node, task: Option<TaskId>, report: Report, at: Instant) {
        let queue = self.queues.entry(node).or_default();
        queue.waiting.retain(|i| {
            i.task != task || !matches!(i.report.kind, ReportKind::Checkpoint | ReportKind::Done)
        });
        queue.waiting.push(Item { task, report, at });
        queue.fresh = true;
    }

    /// When the next batch falls due, if anything waits.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.queues.values().filter_map(Queue::due).min()
    }

    /// The batches due by `now`, each for its node's live terminal as `term_of` names it. What
    /// waits for a node with no live terminal stays. A batch holds what its node had
    /// outstanding too, and replaces it.
    pub(crate) fn take(
        &mut self,
        now: Instant,
        mut term_of: impl FnMut(&Node) -> Option<TermRef>,
    ) -> Vec<Batch> {
        let mut out = Vec::new();
        for queue in self.queues.values_mut() {
            queue.stuck_last.retain(|_, last| now.duration_since(*last) < STUCK_EVERY);
        }
        for (node, queue) in &mut self.queues {
            if queue.due().is_none_or(|due| due > now) {
                continue;
            }
            let Some(term) = term_of(node) else {
                queue.parked = true;
                continue;
            };
            let mut items = queue.outstanding.take().map(|o| o.items).unwrap_or_default();
            items.append(&mut queue.waiting);
            let (context, items, left) = context(&node.0, items);
            // What did not fit waits for the next batch, due as it was.
            queue.waiting = left;
            queue.fresh = false;
            for item in &items {
                if let (Some(task), ReportKind::Stuck) = (item.task, item.report.kind) {
                    queue.stuck_last.insert(task, now);
                }
            }
            self.next_batch = self.next_batch.wrapping_add(1);
            let batch = self.next_batch;
            let reports = u16::try_from(items.len()).unwrap_or(u16::MAX);
            queue.outstanding = Some(Outstanding { batch, term, items });
            out.push(Batch { node: node.clone(), term, number: batch, context, reports });
        }
        self.prune();
        out
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

    /// `term` handed batch `batch` to its agent: the node it was for and how many reports it
    /// held, unless it was replaced since.
    pub(crate) fn acked(&mut self, term: TermRef, batch: u64) -> Option<(Node, u16)> {
        let (node, queue) = self.queues.iter_mut().find(|(_, q)| {
            q.outstanding.as_ref().is_some_and(|o| o.term == term && o.batch == batch)
        })?;
        let done = queue.outstanding.take()?;
        let node = node.clone();
        // Held back only while the batch was out.
        queue.fresh = true;
        self.prune();
        Some((node, u16::try_from(done.items.len()).unwrap_or(u16::MAX)))
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
                    context: context(&node.0, o.items.clone()).0,
                    reports: u16::try_from(o.items.len()).unwrap_or(u16::MAX),
                })
            })
            .collect()
    }

    /// `term` closed: what it was sent and never handed over waits for the node's next
    /// terminal, due as it was.
    pub(crate) fn closed(&mut self, term: TermRef) {
        for queue in self.queues.values_mut() {
            if queue.outstanding.as_ref().is_some_and(|o| o.term == term)
                && let Some(o) = queue.outstanding.take()
            {
                let mut items = o.items;
                items.append(&mut queue.waiting);
                queue.waiting = items;
            }
        }
    }

    /// Drop queues with nothing in them and no block's clock still running.
    fn prune(&mut self) {
        self.queues.retain(|_, q| {
            !q.waiting.is_empty() || q.outstanding.is_some() || !q.stuck_last.is_empty()
        });
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

const fn kind_word(kind: ReportKind) -> &'static str {
    match kind {
        ReportKind::Checkpoint => "checkpoint",
        ReportKind::NeedsInput => "needs input",
        ReportKind::Stuck => "stuck",
        ReportKind::Done => "done",
    }
}

/// The batch as the agent reads it: one block per report, the server's own words first, as
/// many as fit [`CONTEXT_MAX`]. The reports it holds, and those left for the next batch, come
/// back with it.
fn context(project: &ProjectId, mut items: Vec<Item>) -> (String, Vec<Item>, Vec<Item>) {
    items.sort_by_key(|i| (i.task.is_some(), i.at));
    let head = format!("<slopty-reports project=\"{project}\">");
    let tail = "</slopty-reports>";
    let room = CONTEXT_MAX.saturating_sub(head.len()).saturating_sub(tail.len()).saturating_sub(64);
    let mut blocks: Vec<String> = Vec::new();
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
        blocks.push(block);
        taken.push(item);
    }
    let more =
        (!left.is_empty()).then(|| format!("({} more follow once these are read)", left.len()));
    let text = std::iter::once(head)
        .chain(blocks)
        .chain(more)
        .chain(std::iter::once(tail.to_owned()))
        .collect::<Vec<_>>()
        .join("\n");
    (text, taken, left)
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
    let Some(task) = item.task else { return r.note.trim().to_owned() };
    let mut lines = vec![format!("task {task}: {}", kind_word(r.kind))];
    lines.extend(r.note.trim().lines().map(|line| format!("  {}", plain(line))));
    lines.extend(r.branch.iter().map(|branch| format!("  branch: {}", plain(branch))));
    lines.extend(r.pr.iter().map(|pr| format!("  pull request: #{pr}")));
    lines.extend(r.artifacts.iter().map(|artifact| format!("  made: {}", plain(artifact))));
    lines.join("\n")
}

/// `text` with the block's own tag name, in any case, spelled apart: an agent's words that
/// read as `</slopty-reports>` would end the block and pass as the server's.
fn plain(text: &str) -> String {
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
