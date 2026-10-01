//! The fleet's attention ladder, and the notices it sends by where the person is
//! (`slopty_proto::thread::attention`, `docs/decisions/agents.md`).
//!
//! Each worker publishes its thread table ([`ToServer::Threads`]); the hub keeps the rows and
//! ranks them again whenever a row, a terminal or a project moves: a subagent folds into the
//! thread it hangs from, and the rest roll up per tile, worker, project node, project and the
//! fleet. A ladder that differs from the last goes to every link.
//!
//! A thread that hangs from no other and climbs to needing the person, fails, or comes to rest
//! from working, is a notice. It goes to no client when the thread's tile is on screen where
//! the person is, to the desks they are at when they are at one, to the handhelds they hold
//! when not, and to every client when they are at none.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use slopty_core::{WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::server::FromServer;
use slopty_proto::thread::ThreadId;
use slopty_proto::thread::attention::{
    Ladder, NodeAt, Notice, NoticeKind, Presence, Present, Ranked, Rung, Seat, Standing, ThreadAt,
    Via,
};
use slopty_proto::thread::wire::{TableFrame, ThreadRow};
use tokio::sync::{Notify, broadcast, mpsc};

use super::{Hub, WeakHub};
use crate::project::Projects;

/// The rows every worker published, the ladder made of them, and the clients the person may
/// be at.
#[derive(Debug, Default)]
pub(super) struct Board {
    /// Each worker's rows, by thread.
    tables: HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>,
    /// The ladder last published.
    published: Ladder,
    /// Since when each thread has been busy (working or waiting), for how long a finished
    /// one worked.
    busy: HashMap<ThreadAt, WallMs>,
    /// Every person's client link, by the server's number for it.
    seats: BTreeMap<u64, Sitting>,
    /// The last number a link was given.
    next_link: u64,
    /// Wakes [`Hub::publish_ladder`] when rows came.
    wake: Arc<Notify>,
}

/// A person's client link.
#[derive(Debug)]
struct Sitting {
    name: String,
    /// Where its notices go: the link's own queue.
    tx: mpsc::Sender<FromServer>,
    /// Where the person is on it, once it said.
    presence: Option<Presence>,
}

impl Board {
    /// Take in a worker's table frame. A snapshot replaces what it published before.
    pub(super) fn take(&mut self, worker: WorkerId, frame: TableFrame) {
        let table = self.tables.entry(worker).or_default();
        match frame {
            TableFrame::Snapshot { rows, .. } => {
                *table = rows.into_iter().map(|r| (r.id, r)).collect();
            }
            TableFrame::Delta { rows, removed, .. } => {
                for gone in removed {
                    table.remove(&gone);
                }
                table.extend(rows.into_iter().map(|r| (r.id, r)));
            }
        }
        self.wake.notify_one();
    }

    /// Where the person is on every client that said.
    fn present(&self) -> Vec<Present> {
        self.seats
            .iter()
            .filter_map(|(link, s)| {
                let presence = s.presence.clone()?;
                Some(Present { link: *link, name: s.name.clone(), presence })
            })
            .collect()
    }
}

/// A person's client link seated on the hub, for as long as the link lives.
#[derive(Debug)]
pub struct Seated {
    hub: WeakHub,
    link: u64,
}

impl Seated {
    /// The server's number for the link.
    #[must_use]
    pub const fn link(&self) -> u64 {
        self.link
    }
}

impl Drop for Seated {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else { return };
        let mut state = hub.inner.state.lock();
        if state.board.seats.remove(&self.link).is_some_and(|s| s.presence.is_some()) {
            hub.announce(FromServer::Present(state.board.present()));
        }
        drop(state);
    }
}

impl Hub {
    /// A number for a new link, its own among every link this server has had.
    #[must_use]
    pub fn number_link(&self) -> u64 {
        let mut state = self.inner.state.lock();
        state.board.next_link = state.board.next_link.wrapping_add(1);
        state.board.next_link
    }

    /// Seat the person's client link `link` ([`Self::number_link`]), named `name`, whose
    /// notices go on `tx`.
    #[must_use]
    pub fn seat(&self, link: u64, name: String, tx: mpsc::Sender<FromServer>) -> Seated {
        let mut state = self.inner.state.lock();
        state.board.seats.insert(link, Sitting { name, tx, presence: None });
        drop(state);
        Seated { hub: self.downgrade(), link }
    }

    /// Where the person is on the client of `link`.
    pub fn presence(&self, link: u64, presence: Presence) {
        let mut state = self.inner.state.lock();
        let Some(seat) = state.board.seats.get_mut(&link) else { return };
        if seat.presence.as_ref() == Some(&presence) {
            return;
        }
        seat.presence = Some(presence);
        self.announce(FromServer::Present(state.board.present()));
        drop(state);
    }

    /// The ladder as last published.
    #[must_use]
    pub fn ladder(&self) -> Ladder {
        self.inner.state.lock().board.published.clone()
    }

    /// Where the person is on every client that said.
    #[must_use]
    pub fn present(&self) -> Vec<Present> {
        self.inner.state.lock().board.present()
    }

    /// Rank the fleet again whenever rows come or anything else moves, and publish what
    /// changed, until the hub goes.
    pub async fn publish_ladder(hub: WeakHub) {
        let Some((wake, mut events)) = hub.upgrade().map(|h| {
            let wake = Arc::clone(&h.inner.state.lock().board.wake);
            (wake, h.subscribe())
        }) else {
            return;
        };
        loop {
            tokio::select! {
                () = wake.notified() => {}
                event = events.recv() => match event {
                    Ok(FromServer::Load { .. } | FromServer::Ladder(_) | FromServer::Present(_)) => {
                        continue;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
            // Whatever else came meanwhile is ranked in the same pass.
            while !matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)
            ) {}
            let Some(hub) = hub.upgrade() else { return };
            hub.rank_ladder();
        }
    }

    /// Rank every thread now, and publish the ladder and its notices when it moved.
    pub(super) fn rank_ladder(&self) {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let live = &state.workers;
        state.board.tables.retain(|worker, _| live.contains_key(worker));
        let ladder = ladder(&state.board.tables, &state.projects);
        if ladder == state.board.published {
            return;
        }
        let notices = moved(&mut state.board, &ladder);
        self.announce(FromServer::Ladder(Box::new(ladder.clone())));
        state.board.published = ladder;
        for notice in notices {
            for link in route(&state.board.seats, &notice) {
                let Some(seat) = state.board.seats.get(&link) else { continue };
                if seat.tx.try_send(FromServer::Notice(Box::new(notice.clone()))).is_err() {
                    tracing::debug!(link, "a notice found its link full or gone");
                }
            }
        }
        drop(guard);
    }
}

/// A thread that hangs from no other, standing as high as its subagents.
struct Root<'a> {
    ranked: Ranked,
    row: &'a ThreadRow,
}

/// The thread `row` hangs from in `table`, through every parent there; itself when none.
fn root_of(table: &BTreeMap<ThreadId, ThreadRow>, row: &ThreadRow) -> ThreadId {
    let mut at = row.id;
    // A parent chain longer than the table loops: it ends where it started over.
    for _ in 0..table.len() {
        match table.get(&at).and_then(|r| r.parent.as_ref()).map(|p| p.thread) {
            Some(parent) if table.contains_key(&parent) => at = parent,
            _ => break,
        }
    }
    at
}

/// Where `family`, hanging from `root`, stands together, and the row that puts it there: the
/// root's own when it stands highest, else the subagent there longest.
///
/// A subagent that needs the person always lifts its root. One that failed reads as working
/// while anyone in the family still works or waits, since its parent may well carry on
/// without it, and lifts the root to failed only once the whole family is at rest.
fn source<'a>(root: &'a ThreadRow, family: &[&'a ThreadRow]) -> (Rung, &'a ThreadRow) {
    let busy = std::iter::once(root)
        .chain(family.iter().copied())
        .any(|r| matches!(Rung::of(r), Rung::Working | Rung::Waiting));
    let rung = |row: &ThreadRow| match Rung::of(row) {
        Rung::Failed if busy && row.id != root.id => Rung::Working,
        rung => rung,
    };
    let top = family.iter().map(|r| rung(r)).max().unwrap_or_default().max(rung(root));
    if rung(root) == top {
        return (top, root);
    }
    let from = family
        .iter()
        .copied()
        .filter(|r| rung(r) == top)
        .min_by_key(|r| (r.status.since_ms, r.id))
        .unwrap_or(root);
    (top, from)
}

/// Every thread of `table` that hangs from no other, with its subagents.
fn families(table: &BTreeMap<ThreadId, ThreadRow>) -> BTreeMap<ThreadId, Vec<&ThreadRow>> {
    let mut families: BTreeMap<ThreadId, Vec<&ThreadRow>> = BTreeMap::new();
    for row in table.values() {
        let root = root_of(table, row);
        let family = families.entry(root).or_default();
        if root != row.id {
            family.push(row);
        }
    }
    families
}

/// Every worker's threads that hang from no other, each standing as high as its subagents.
fn roots(tables: &HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>) -> Vec<Root<'_>> {
    let mut roots: Vec<Root<'_>> = tables
        .iter()
        .flat_map(|(worker, table)| {
            families(table).into_iter().filter_map(|(root, family)| {
                let row = table.get(&root)?;
                let (rung, from) = source(row, &family);
                let at = ThreadAt { worker: *worker, thread: root };
                let ranked = Ranked { at, rung, since_ms: from.status.since_ms };
                Some(Root { ranked, row })
            })
        })
        .collect();
    roots.sort_by_key(|r| r.ranked.at);
    roots
}

/// The project nodes `term` works under: its task and every task above it, or the project's
/// orchestrator; and the project.
fn nodes_of(projects: &Projects, term: TermRef) -> Vec<NodeAt> {
    let Some((project, task)) = projects.working_on(term) else { return Vec::new() };
    let mut nodes = vec![NodeAt { project: project.clone(), task }];
    let mut at = task;
    while let Some(task) = at {
        let parent = projects.task(&project, task).ok().and_then(|t| t.parent);
        if let Some(parent) = parent {
            if nodes.iter().any(|n| n.task == Some(parent)) {
                break;
            }
            nodes.push(NodeAt { project: project.clone(), task: Some(parent) });
        }
        at = parent;
    }
    nodes
}

/// The ladder of `tables`, with the project nodes `projects` puts their terminals under.
fn ladder(
    tables: &HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>,
    projects: &Projects,
) -> Ladder {
    let roots = roots(tables);
    let mut tiles: BTreeMap<(WorkerId, slopty_core::SessionId), Standing> = BTreeMap::new();
    let mut workers: BTreeMap<WorkerId, Standing> = BTreeMap::new();
    let mut nodes: BTreeMap<NodeAt, Standing> = BTreeMap::new();
    let mut by_project: BTreeMap<slopty_proto::project::ProjectId, Standing> = BTreeMap::new();
    let mut fleet = Standing::default();
    for root in &roots {
        let ranked = &root.ranked;
        fleet.add(ranked);
        workers.entry(ranked.at.worker).or_default().add(ranked);
        let Some(session) = root.row.terminal else { continue };
        tiles.entry((ranked.at.worker, session)).or_default().add(ranked);
        let term = TermRef { worker: ranked.at.worker, session };
        let under = nodes_of(projects, term);
        if let Some(first) = under.first() {
            by_project.entry(first.project.clone()).or_default().add(ranked);
        }
        for node in under {
            nodes.entry(node).or_default().add(ranked);
        }
    }
    Ladder {
        threads: roots.iter().map(|r| r.ranked).collect(),
        tiles: tiles
            .into_iter()
            .map(|((worker, session), s)| (TermRef { worker, session }, s))
            .collect(),
        workers: workers.into_iter().collect(),
        nodes: nodes.into_iter().collect(),
        projects: by_project.into_iter().collect(),
        fleet,
    }
}

/// The notices `ladder` makes against the one `board` published, keeping how long each
/// thread has been busy.
fn moved(board: &mut Board, ladder: &Ladder) -> Vec<Notice> {
    let before: HashMap<ThreadAt, Rung> =
        board.published.threads.iter().map(|r| (r.at, r.rung)).collect();
    let busy = |rung: Rung| matches!(rung, Rung::Working | Rung::Waiting);
    let mut notices = Vec::new();
    for now in &ladder.threads {
        let was = before.get(&now.at).copied();
        if busy(now.rung) {
            board.busy.entry(now.at).or_insert(now.since_ms);
        }
        // A wait on the person is part of the work; coming to rest or failing ends it.
        let rest = matches!(now.rung, Rung::ToReview | Rung::Idle | Rung::Failed);
        let kind = match (was, now.rung) {
            (Some(was), Rung::NeedsYou) if was != Rung::NeedsYou => Some(NoticeKind::NeedsYou),
            (Some(was), Rung::Failed) if was != Rung::Failed => Some(NoticeKind::Failed),
            (Some(was), Rung::ToReview | Rung::Idle) if busy(was) => Some(NoticeKind::Finished),
            _ => None,
        };
        let worked_ms = if rest {
            let since = board.busy.remove(&now.at);
            since.map(|since| now.since_ms.as_millis().saturating_sub(since.as_millis()))
        } else {
            None
        };
        let Some(kind) = kind else { continue };
        let Some(table) = board.tables.get(&now.at.worker) else { continue };
        let Some(row) = table.get(&now.at.thread) else { continue };
        let family: Vec<&ThreadRow> =
            table.values().filter(|r| r.id != row.id && root_of(table, r) == row.id).collect();
        let (_, from) = source(row, &family);
        let via = (from.id != row.id).then(|| Via { thread: from.id, title: from.title.clone() });
        notices.push(Notice {
            kind,
            thread: now.at,
            tile: row.terminal,
            title: row.title.clone(),
            text: text(kind, from),
            worked_ms: (kind == NoticeKind::Finished).then_some(worked_ms).flatten(),
            via,
        });
    }
    let standing: Vec<ThreadAt> = ladder.threads.iter().map(|r| r.at).collect();
    board.busy.retain(|at, _| standing.binary_search(at).is_ok());
    notices
}

/// What a notice of `kind` says of `row`, in a line.
fn text(kind: NoticeKind, row: &ThreadRow) -> String {
    let wait = row.status.wait.as_ref().map(|w| w.text.clone()).filter(|t| !t.is_empty());
    match kind {
        NoticeKind::NeedsYou => {
            row.requests.first().map(|r| r.title.clone()).or(wait).unwrap_or_default()
        }
        NoticeKind::Failed | NoticeKind::Finished => {
            wait.or_else(|| row.last_line.clone()).unwrap_or_default()
        }
    }
}

/// The links `notice` goes to among `seats`.
fn route(seats: &BTreeMap<u64, Sitting>, notice: &Notice) -> Vec<u64> {
    let tile = notice.tile.map(|session| TermRef { worker: notice.thread.worker, session });
    let at = |seat: Option<Seat>| {
        seats
            .iter()
            .filter(|(_, s)| {
                s.presence.as_ref().is_some_and(|p| p.active && seat.is_none_or(|k| p.seat == k))
            })
            .map(|(link, _)| *link)
            .collect::<Vec<_>>()
    };
    let shown = seats
        .values()
        .filter_map(|s| s.presence.as_ref())
        .any(|p| p.active && tile.is_some_and(|t| p.showing.contains(&t) || p.focus == Some(t)));
    if shown {
        return Vec::new();
    }
    let desks = at(Some(Seat::Desk));
    if !desks.is_empty() {
        return desks;
    }
    let held = at(Some(Seat::Handheld));
    if !held.is_empty() {
        return held;
    }
    seats.keys().copied().collect()
}

#[cfg(test)]
mod tests;
