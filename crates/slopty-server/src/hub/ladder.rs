//! The fleet's attention ladder, and the notices it sends by where the person is
//! (`slopty_proto::thread::attention`, `docs/decisions/agents.md`).
//!
//! Each worker publishes its thread table
//! ([`slopty_proto::server::ToServer::Threads`]); the hub keeps the rows and ranks them again
//! whenever a row, a terminal or a project moves: a subagent folds into the thread it hangs from,
//! and the rest roll up per tile, worker, project node, project and the fleet. A ladder that
//! differs from the last goes to every link.
//!
//! A thread that hangs from no other and climbs to needing the person, fails, or comes to rest
//! from working, is a notice. It goes to no client when the thread's tile is on screen where
//! the person is, to the desks they are at when they are at one, to the handhelds they hold
//! when not, and to every client when they are at none.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::{AgentStatus, BlockReason};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{AgentReport, SEAT_FACT};
use slopty_proto::server::FromServer;
use slopty_proto::thread::attention::{
    Ladder, NodeAt, Notice, NoticeKind, Presence, Present, Ranked, Rung, Seat, Standing, ThreadAt,
    Via,
};
use slopty_proto::thread::wire::{TableFrame, ThreadRow};
use slopty_proto::thread::{AgentId, Liveness, Phase, Request, ThreadId};
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
    /// What each task's thread with no terminal of its own was last said to be doing, by its
    /// seat ([`Self::seat_moves`]).
    seat_said: HashMap<TermRef, AgentStatus>,
    /// Every task's thread a table has shown, by its worker: one gone from it since ended.
    seen: std::collections::HashSet<(WorkerId, ThreadId)>,
    /// Each subagent thread last told to the projects as a native ([`Self::native_moves`]),
    /// with the seat it was told under and whether it had stopped.
    natives_said: HashMap<(WorkerId, ThreadId), (SessionId, bool)>,
}

/// What one thread says it spent so far, under the terminal its agent runs in.
#[derive(Debug)]
pub(super) struct Figures {
    /// The terminal.
    pub term: TermRef,
    /// The thread.
    pub thread: ThreadId,
    /// Its estimated cost, in millionths of a US dollar, when its agent says one.
    pub cost_micro_usd: Option<u64>,
    /// The plan's rate windows as it last read them.
    pub windows: Vec<slopty_proto::thread::Limit>,
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
    /// The ladder last published.
    pub(super) const fn published(&self) -> &Ladder {
        &self.published
    }

    /// Take in a worker's table frame. A snapshot replaces what it published before.
    ///
    /// Answers what each thread the frame names says it spent, under the terminal its agent
    /// runs in: a subagent's under its root's, since the project knows the root's terminal.
    /// A thread whose family runs in no terminal says nothing.
    pub(super) fn take(&mut self, worker: WorkerId, frame: TableFrame) -> Vec<Figures> {
        let table = self.tables.entry(worker).or_default();
        let named: Vec<ThreadId> = match frame {
            TableFrame::Snapshot { rows, .. } => {
                *table = rows.into_iter().map(|r| (r.id, r)).collect();
                table.keys().copied().collect()
            }
            TableFrame::Delta { rows, removed, .. } => {
                for gone in removed {
                    table.remove(&gone);
                }
                let named = rows.iter().map(|r| r.id).collect();
                table.extend(rows.into_iter().map(|r| (r.id, r)));
                named
            }
        };
        self.wake.notify_one();
        self.seen.extend(table.values().filter(|r| seat_fact(r).is_some()).map(|r| (worker, r.id)));
        named
            .into_iter()
            .filter_map(|id| {
                let row = table.get(&id)?;
                let root = table.get(&root_of(table, row))?;
                let session = seat_of(row).or_else(|| seat_of(root))?;
                Some(Figures {
                    term: TermRef { worker, session },
                    thread: id,
                    cost_micro_usd: row.meters.cost_micro_usd,
                    windows: row.meters.limits.clone(),
                })
            })
            .collect()
    }

    /// The thread seated at `term` (its TUI runs there, or a task's thread was started there)
    /// and hanging from no other, the latest to change when there were several.
    fn thread_in(&self, term: TermRef) -> Option<&ThreadRow> {
        let table = self.tables.get(&term.worker)?;
        table
            .values()
            .filter(|r| seat_of(r) == Some(term.session) && root_of(table, r) == r.id)
            .max_by_key(|r| (r.updated_ms, r.id))
    }

    /// The thread whose agent runs or is seated at `term`, hanging from no other.
    pub(super) fn thread_at(&self, term: TermRef) -> Option<ThreadId> {
        self.thread_in(term).map(|r| r.id)
    }

    /// The worker whose table holds `thread`.
    pub(super) fn worker_of(&self, thread: ThreadId) -> Option<WorkerId> {
        self.tables.iter().find(|(_, table)| table.contains_key(&thread)).map(|(w, _)| *w)
    }

    /// The seat `session` of a task's thread on any worker, as its row's [`SEAT_FACT`] says.
    pub(super) fn seat(&self, session: SessionId) -> Option<TermRef> {
        self.tables.iter().find_map(|(worker, table)| {
            table
                .values()
                .any(|r| seat_fact(r) == Some(session))
                .then_some(TermRef { worker: *worker, session })
        })
    }

    /// The thread a task's start seated at `term`, as its row's [`SEAT_FACT`] says.
    pub(super) fn seated_thread(&self, term: TermRef) -> Option<ThreadId> {
        let table = self.tables.get(&term.worker)?;
        table.values().find(|r| seat_fact(r) == Some(term.session)).map(|r| r.id)
    }

    /// Every task's thread with no terminal of its own whose agent runs, by its seat: it
    /// counts as a live terminal does. One asleep does not, since its agent ended.
    pub(super) fn live_seats(&self) -> Vec<TermRef> {
        self.tables
            .iter()
            .flat_map(|(worker, table)| {
                table
                    .values()
                    .filter(|r| r.terminal.is_none() && awake(r))
                    .filter_map(|r| Some(TermRef { worker: *worker, session: seat_fact(r)? }))
            })
            .collect()
    }

    /// Whether the thread `thread` on `worker` is in its table with its agent there; `None`
    /// when the worker has published no table.
    pub(super) fn thread_there(&self, worker: WorkerId, thread: ThreadId) -> Option<bool> {
        let table = self.tables.get(&worker)?;
        Some(table.get(&thread).is_some_and(there))
    }

    /// Whether a table of `worker`'s has shown the task's thread `thread`.
    pub(super) fn seen(&self, worker: WorkerId, thread: ThreadId) -> bool {
        self.seen.contains(&(worker, thread))
    }

    /// What the agent of the task's thread seated at `term`, with no terminal of its own, is
    /// doing, as an agent's status reads: what a terminal's agent says through its hooks.
    pub(super) fn seat_status(&self, term: TermRef) -> Option<AgentStatus> {
        let row = self.thread_in(term).filter(|r| r.terminal.is_none())?;
        Some(status_of(row))
    }

    /// Each task's thread on `worker` with no terminal of its own whose status moved since
    /// last asked, with its status now; one gone is forgotten.
    pub(super) fn seat_moves(&mut self, worker: WorkerId) -> Vec<(TermRef, AgentStatus)> {
        let now: Vec<(TermRef, AgentStatus)> = self
            .tables
            .get(&worker)
            .map(|table| {
                table
                    .values()
                    .filter(|r| r.terminal.is_none() && root_of(table, r) == r.id)
                    .filter_map(|r| {
                        Some((TermRef { worker, session: seat_fact(r)? }, status_of(r)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.seat_said
            .retain(|term, _| term.worker != worker || now.iter().any(|(seat, _)| seat == term));
        now.into_iter()
            .filter(|(term, status)| {
                self.seat_said.insert(*term, status.clone()).as_ref() != Some(status)
            })
            .collect()
    }

    /// Each subagent thread on `worker` that started or stopped since last asked, as the
    /// report a hook would make of it under the seat its family runs at: the natives of
    /// every agent but Claude Code, whose own hooks report its subagents. A subagent gone
    /// from the table stopped.
    pub(super) fn native_moves(&mut self, worker: WorkerId) -> Vec<AgentReport> {
        let mut reports = Vec::new();
        let table = self.tables.get(&worker);
        let now: Vec<(ThreadId, SessionId, &ThreadRow)> = table
            .map(|table| {
                table
                    .values()
                    .filter_map(|row| {
                        let root = table.get(&root_of(table, row)).filter(|r| r.id != row.id)?;
                        let hooked = root.agent == AgentId::named(AgentId::CLAUDE_CODE);
                        Some((row.id, seat_of(root).filter(|_| !hooked)?, row))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (id, session, row) in &now {
            let at_work =
                matches!(row.status.phase, Phase::Working | Phase::Waiting | Phase::NeedsYou);
            let stopped = !awake(row) || !at_work;
            let said = self.natives_said.insert((worker, *id), (*session, stopped));
            let agent = id.to_string();
            if said.is_none() {
                let kind = Some(row.title.trim()).filter(|t| !t.is_empty());
                let kind = kind.map_or_else(|| row.agent.0.clone(), str::to_owned);
                let session = *session;
                reports.push(AgentReport::SubagentStarted { session, agent: agent.clone(), kind });
            }
            if stopped && said.is_none_or(|(_, was)| !was) {
                let last = row.last_line.clone().filter(|l| !l.trim().is_empty());
                let session = *session;
                reports.push(AgentReport::SubagentStopped {
                    session,
                    agent,
                    transcript: None,
                    last,
                });
            }
        }
        self.natives_said.retain(|(w, id), (session, stopped)| {
            if *w != worker || now.iter().any(|(t, ..)| t == id) {
                return true;
            }
            if !*stopped {
                let (session, agent) = (*session, id.to_string());
                reports.push(AgentReport::SubagentStopped {
                    session,
                    agent,
                    transcript: None,
                    last: None,
                });
            }
            false
        });
        reports
    }

    /// The last line the agent in `term` wrote, as its thread's row says.
    pub(super) fn last_words(&self, term: TermRef) -> Option<String> {
        self.thread_in(term)?.last_line.clone().filter(|l| !l.trim().is_empty())
    }

    /// What the agent in `term` asks the person, as the first open request on its thread's
    /// row names it.
    pub(super) fn asking(&self, term: TermRef) -> Option<String> {
        let row = self.thread_in(term)?;
        row.requests.first().map(|r| r.title.clone()).filter(|t| !t.trim().is_empty())
    }

    /// Whether any thread seated at `term`, or under one there, has a request open.
    pub(super) fn asks(&self, term: TermRef) -> bool {
        self.tables.get(&term.worker).is_some_and(|table| {
            table.values().any(|r| {
                !r.requests.is_empty()
                    && table.get(&root_of(table, r)).and_then(seat_of) == Some(term.session)
            })
        })
    }

    /// Whether `term`'s tile is on screen, or has the keyboard, on any client.
    pub(super) fn shown(&self, term: TermRef) -> bool {
        self.seats
            .values()
            .filter_map(|s| s.presence.as_ref())
            .any(|p| p.showing.contains(&term) || p.focus == Some(term))
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
        self.snoozes_heard(state, &notices);
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
/// Where `row`'s thread sits, as the projects know its agent: the terminal its TUI runs in,
/// else the seat a task's thread was started at ([`SEAT_FACT`]).
fn seat_of(row: &ThreadRow) -> Option<SessionId> {
    row.terminal.or_else(|| seat_fact(row))
}

/// The seat a task's thread was started at, as its row's [`SEAT_FACT`] says.
fn seat_fact(row: &ThreadRow) -> Option<SessionId> {
    row.facts.get(SEAT_FACT)?.parse().ok()
}

/// Whether `row`'s thread is still to be had: its process has not ended, or it sleeps on the
/// person's word with its session kept, to wake on the next message.
const fn there(row: &ThreadRow) -> bool {
    !matches!(row.status.liveness, Liveness::Exited { .. })
}

/// Whether `row`'s agent runs now: there, and not put to sleep, so it takes a place among
/// the live agents.
const fn awake(row: &ThreadRow) -> bool {
    there(row) && !matches!(row.status.liveness, Liveness::Asleep { .. })
}

/// `row`'s phase as an agent's status reads, for a task's thread with no hooks: a request
/// open is a block on the person, by what it asks.
fn status_of(row: &ThreadRow) -> AgentStatus {
    match row.status.phase {
        Phase::Working => AgentStatus::Working,
        Phase::Waiting => AgentStatus::Waiting { tasks: 1, crons: 0 },
        Phase::NeedsYou => AgentStatus::Blocked(match row.requests.first() {
            Some(r) if r.kind == Request::APPROVAL => {
                BlockReason::Permission { tool: r.title.clone() }
            }
            Some(r) if r.kind == Request::ELICITATION => BlockReason::Elicitation,
            _ => BlockReason::Question,
        }),
        Phase::Done => AgentStatus::Done,
        // The row names no error; the turn it ended is said by the outcome notice.
        Phase::Failed => AgentStatus::Failed { error: "unknown".to_owned(), until_ms: None },
        Phase::Idle | Phase::Stopped => AgentStatus::Idle,
    }
}

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
    // From the root's own rung: no default stands in, since asleep stands below idle.
    let top = family.iter().map(|r| rung(r)).fold(rung(root), Ord::max);
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
                let ranked =
                    Ranked { at, rung, since_ms: from.status.since_ms, terminal: row.terminal };
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
    let mut tiles: BTreeMap<(WorkerId, SessionId), Standing> = BTreeMap::new();
    let mut workers: BTreeMap<WorkerId, Standing> = BTreeMap::new();
    let mut nodes: BTreeMap<NodeAt, Standing> = BTreeMap::new();
    let mut by_project: BTreeMap<slopty_proto::project::ProjectId, Standing> = BTreeMap::new();
    let mut fleet = Standing::default();
    for root in &roots {
        let ranked = &root.ranked;
        fleet.add(ranked);
        workers.entry(ranked.at.worker).or_default().add(ranked);
        if let Some(session) = root.row.terminal {
            tiles.entry((ranked.at.worker, session)).or_default().add(ranked);
        }
        let Some(session) = seat_of(root.row) else { continue };
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
        // A wait on the person is part of the work; coming to rest or failing ends it, as
        // being put to sleep does.
        let rest = matches!(now.rung, Rung::ToReview | Rung::Idle | Rung::Failed | Rung::Sleeping);
        let kind = match (was, now.rung) {
            (Some(was), Rung::NeedsYou) if was != Rung::NeedsYou => Some(NoticeKind::NeedsYou),
            (Some(was), Rung::Failed) if was != Rung::Failed => Some(NoticeKind::Failed),
            (Some(was), Rung::ToReview | Rung::Idle) if busy(was) => Some(NoticeKind::Finished),
            // The person puts an agent to sleep at rest, so its turn finished before, and was
            // told then: a second notice would tell nothing new.
            #[expect(
                clippy::match_same_arms,
                reason = "sleep is ruled on its own, so a rung added later is not silently quiet"
            )]
            (_, Rung::Sleeping) => None,
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
pub(super) mod tests;
