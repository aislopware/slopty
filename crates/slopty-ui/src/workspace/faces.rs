//! Every worker's threads: one [`ThreadHub`] per worker, fed by the link
//! ([`WorkspaceView::threads_linked`], [`WorkspaceView::thread_table`],
//! [`WorkspaceView::thread_frame`], [`WorkspaceView::thread_done`]), and what their tables last
//! said of each thread: its title, its place and where it stands.
//!
//! An aside ([`hub::is_aside`]) is the shell's own business: it is no row here.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{App, AppContext as _, Context, Entity};
use slopty_client::WorkerKey;
use slopty_proto::RequestId;
use slopty_proto::git::GitOutcome;
use slopty_proto::thread::attention::{Ladder, Rung};
use slopty_proto::thread::wire::{IntentDone, RequestCard, TableFrame, ThreadFrame, ThreadRow};
use slopty_proto::thread::{self, AgentId, ThreadId, TurnId};

use super::{WorkspaceEvent, WorkspaceView};
use crate::conversation::thread::{HubEvent, ThreadHub, hub};

/// What the workspace keeps of every worker's threads.
#[derive(Default)]
pub(super) struct Faces {
    /// The hubs and what their tables last said.
    pub threads: ThreadFaces,
}

/// Where a thread works, as its row says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ThreadPlace {
    /// Its directory.
    pub cwd: Option<String>,
    /// The repository that directory is in.
    pub repo: Option<String>,
    /// Which repository that is on every machine.
    pub repo_id: Option<slopty_proto::terminal::RepoId>,
    /// Its branch's pull request, as its worker last read it.
    pub pull: Option<thread::wire::PullSeen>,
}

/// Each worker's threads, and the thread view of each agent tile that shows one.
#[derive(Default)]
pub(super) struct ThreadFaces {
    /// Each worker's threads, made the first time its link comes up.
    hubs: HashMap<WorkerKey, Entity<ThreadHub>>,
    hearing: HashMap<WorkerKey, gpui::Subscription>,
    /// The workers whose table was heard before: a thread new to one of them that already
    /// needs the person comes to need them now, where a first table only says how things are.
    heard: HashSet<WorkerKey>,
    /// Where each worker's threads are kept: a directory per worker under it.
    cache: Option<PathBuf>,
    /// Each thread's title as its worker's table last said, for its tile's header.
    titles: HashMap<ThreadId, String>,
    /// The last line each thread's agent wrote as its worker's table last said, for the note
    /// on a turn that ended unwatched.
    lines: HashMap<ThreadId, String>,
    /// Each thread's agent as its worker's table last said, for the mark its tile leads with.
    agents: HashMap<ThreadId, AgentId>,
    /// Each thread's open facts as its worker's table last said (its project, its task), for
    /// the group its tile is in.
    facts: HashMap<ThreadId, std::collections::BTreeMap<String, String>>,
    /// Where each thread works as its worker's table last said (its directory, the repository
    /// that is in), for the project of a thread tile.
    places: HashMap<ThreadId, ThreadPlace>,
    /// Where each thread stands as its worker's table last said, its subagents folded in, for
    /// the chrome outside its tile.
    stands: HashMap<ThreadId, ThreadStand>,
    /// Where each thread stands as the server's ladder last said: what speaks for the threads
    /// of a worker this client has no link to.
    server: HashMap<ThreadId, ThreadStand>,
    /// Each machine's plan windows as its agents' rows last said them, for the cluster.
    meters: slopty_client::meters::PlanMeters,
}

impl ThreadFaces {
    /// Forget a machine's plan windows, as it goes from the workspace.
    pub(super) fn forget_meters(&mut self, worker: WorkerKey) {
        self.meters.forget(worker);
    }

    // slopty: an agent's plan windows on the Agents page.
    /// The windows `agent` said last on any machine, those still holding at `now`, and when it
    /// said them; `None` while it has said none.
    pub(in crate::workspace) fn usage_of(
        &self,
        agent: &str,
        now: slopty_core::WallMs,
    ) -> Option<slopty_view_proto::AgentUsage> {
        let (_, _, reading) = self
            .meters
            .all(now)
            .filter(|(_, name, _)| *name == agent)
            .max_by_key(|(_, _, reading)| reading.heard)?;
        let holding: Vec<_> = reading.current(now).cloned().collect();
        Some(slopty_view_proto::AgentUsage {
            windows: crate::view_model::rows::limits(&holding),
            fetched_at: Some(crate::view_model::rows::time(reading.heard)),
        })
    }
}

impl WorkspaceView {
    /// A thread's name, the one every surface calls it by: a header, a note, and who wrote a
    /// line. Where its agent runs in a tile here, the
    /// tile's title (the name the person gave it, its project role, or what the agent titled
    /// itself); else its title as its worker's table says it, which every agent's adapter
    /// fills alike (the agent's own name for the session, else its first prompt); else, before
    /// either, its agent's name, as a chat is called before its summary.
    #[must_use]
    pub fn thread_title(&self, thread: ThreadId) -> String {
        self.thread_named(thread)
            .or_else(|| {
                let agent = AgentId::named(self.thread_agent(thread)?);
                Some(hub::agent_label(&agent))
            })
            .unwrap_or_else(|| THREAD.to_owned())
    }

    /// Where `thread` works, as its worker's table last said.
    pub(super) fn thread_place(&self, thread: ThreadId) -> Option<&ThreadPlace> {
        self.faces.threads.places.get(&thread)
    }

    /// `thread`'s title as its worker's table last said, while it has one.
    pub(super) fn thread_named(&self, thread: ThreadId) -> Option<String> {
        self.faces.threads.titles.get(&thread).filter(|t| !t.trim().is_empty()).cloned()
    }

    /// The last line `thread`'s agent wrote, as its worker's table last said.
    pub(super) fn thread_line(&self, thread: ThreadId) -> Option<&str> {
        self.faces.threads.lines.get(&thread).map(String::as_str)
    }

    /// The agent that runs `thread`, by its [`AgentId`] name, as its worker's table last said.
    pub(super) fn thread_agent(&self, thread: ThreadId) -> Option<&str> {
        self.faces.threads.agents.get(&thread).map(|a| a.0.as_str())
    }

    /// Keep each worker's threads in `dir`, a directory per worker.
    pub fn set_thread_cache(&mut self, dir: PathBuf) {
        self.faces.threads.cache = Some(dir);
    }

    /// `key`'s threads, once they have been made.
    pub(super) fn held_hub(&self, key: WorkerKey) -> Option<&Entity<ThreadHub>> {
        self.faces.threads.hubs.get(&key)
    }

    /// Every worker's threads made so far.
    pub(super) fn hubs(&self) -> impl Iterator<Item = (WorkerKey, &Entity<ThreadHub>)> {
        self.faces.threads.hubs.iter().map(|(key, hub)| (*key, hub))
    }

    /// The worker whose table holds `thread`.
    pub(super) fn worker_of_thread(&self, thread: ThreadId, cx: &App) -> Option<WorkerKey> {
        self.faces
            .threads
            .hubs
            .iter()
            .find(|(_, hub)| hub.read(cx).threads().rows().rows.contains_key(&thread))
            .map(|(key, _)| *key)
    }

    /// `key`'s threads, made the first time they are asked for.
    pub(super) fn thread_hub(
        &mut self,
        key: WorkerKey,
        cx: &mut Context<Self>,
    ) -> Entity<ThreadHub> {
        if let Some(hub) = self.faces.threads.hubs.get(&key) {
            return hub.clone();
        }
        let name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
        let cache = self
            .faces
            .threads
            .cache
            .as_ref()
            .map(|dir| slopty_client::threads::Cache::new(dir.join(key.to_string())));
        let hub = cx.new(|_| ThreadHub::new(name, cache));
        let hearing = cx.subscribe(&hub, move |this, _hub, event: &HubEvent, cx| match event {
            HubEvent::Send(msgs) => {
                for msg in msgs {
                    this.send(key, msg.clone());
                }
            }
            HubEvent::Table => this.threads_heard(key, cx),
        });
        self.faces.threads.hearing.insert(key, hearing);
        self.faces.threads.hubs.insert(key, hub.clone());
        self.hub_agents(key, cx);
        hub
    }

    /// `key`'s answer to a git op its hub asked (the commit sheet, the pull request view).
    pub fn git_done(
        &mut self,
        key: WorkerKey,
        request: RequestId,
        outcome: GitOutcome,
        cx: &mut Context<Self>,
    ) {
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.git_done(request, outcome, cx));
    }

    /// The link to `key` is up: its threads catch up from where they stand, the starts sent
    /// there and not answered are sent again as they were, to be taken up where they stand,
    /// its drafts ask for their folders' branches, and the folders its agents' past sessions
    /// ran in are asked for, for the starts to offer.
    pub fn threads_linked(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let hub = self.thread_hub(key, cx);
        hub.update(cx, ThreadHub::connected);
        self.view_resend(key);
        // slopty: the pages waiting for this machine ask again.
        #[cfg(target_vendor = "apple")]
        self.pages_linked(key, cx);
    }

    /// Tell `key`'s thread hub what that machine can start now ("Continue in…"), if it has
    /// one: when the hub is made, and when the machine's caps or link move.
    pub(super) fn hub_agents(&self, key: WorkerKey, cx: &mut Context<Self>) {
        if let Some(hub) = self.faces.threads.hubs.get(&key).cloned() {
            let agents = self.startable_on(key);
            hub.update(cx, |hub, cx| hub.set_agents(agents, cx));
        }
    }

    /// The link to `key` went: its thread views show what they last knew. A start sent there
    /// goes on on the machine, and is kept to be taken up when the link is back.
    pub fn threads_unlinked(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        if let Some(hub) = self.faces.threads.hubs.get(&key) {
            hub.update(cx, ThreadHub::disconnected);
        }
        // slopty: the pages waiting for this machine say so.
        #[cfg(target_vendor = "apple")]
        self.pages_unlinked(key, cx);
    }

    /// A frame of `key`'s thread table.
    pub fn thread_table(&mut self, key: WorkerKey, frame: &TableFrame, cx: &mut Context<Self>) {
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.table(frame, cx));
    }

    /// A frame of one of `key`'s threads.
    pub fn thread_frame(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        frame: ThreadFrame,
        cx: &mut Context<Self>,
    ) {
        // slopty: an agent's answer to what an inline page asked of its app goes to the page.
        #[cfg(target_vendor = "apple")]
        if let ThreadFrame::App { id, answer } = frame {
            self.page_app_answer(id, answer, cx);
            return;
        }
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.frame(thread, frame, cx));
    }

    /// `key`'s answer to one of this client's intents. A start answered with its thread
    /// opens the thread as a tile there; one refused says why.
    pub fn thread_done(&mut self, key: WorkerKey, done: &IntentDone, cx: &mut Context<Self>) {
        self.view_intent_done(done);
        if let Some(hub) = self.faces.threads.hubs.get(&key) {
            hub.update(cx, |hub, cx| hub.done(done, cx));
        }
    }

    /// What `key`'s table says of its threads: where each stands, its title, place and agent.
    /// A subagent's thread is its parent's business, and an aside its asker's.
    pub(super) fn threads_heard(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let Some(hub) = self.faces.threads.hubs.get(&key) else { return };
        let rows: Vec<&ThreadRow> =
            hub.read(cx).threads().rows().rows.values().filter(|r| !hub::is_aside(r)).collect();
        let stands = stands_of(key, rows.iter().copied());
        let old = &self.faces.threads.stands;
        let moved = old.values().filter(|s| s.worker == key).count() != stands.len()
            || stands.iter().any(|(id, stand)| old.get(id) != Some(stand));
        // A thread that comes to need the person, since the worker's table was first heard:
        // the corner may point at it.
        let heard = self.faces.threads.heard.contains(&key);
        let came_to_need: Vec<ThreadId> = stands
            .iter()
            .filter(|(id, stand)| {
                stand.rung == Rung::NeedsYou
                    && old.get(*id).map_or(heard, |was| was.rung != Rung::NeedsYou)
            })
            .map(|(id, _)| *id)
            .collect();
        self.faces.threads.stands.retain(|_, stand| stand.worker != key);
        self.faces.threads.stands.extend(stands);
        self.faces.threads.heard.insert(key);
        let meters_before = self.faces.threads.meters.clone();
        for row in &rows {
            self.faces.threads.titles.insert(row.id, row.title.clone());
            match &row.last_line {
                Some(line) => self.faces.threads.lines.insert(row.id, line.clone()),
                None => self.faces.threads.lines.remove(&row.id),
            };
            self.faces.threads.agents.insert(row.id, row.agent.clone());
            self.faces.threads.facts.insert(row.id, row.facts.clone());
            let place = ThreadPlace {
                cwd: row.cwd.clone(),
                repo: row.repo.clone(),
                repo_id: row.repo_id.clone(),
                pull: row.pull.clone(),
            };
            self.faces.threads.places.insert(row.id, place);
            self.faces.threads.meters.hear(key, &row.agent.0, &row.meters.limits, row.updated_ms);
        }
        self.turns_heard(key, !heard, cx);
        let linked = self.workers.get(&key).is_some_and(|w| w.link.is_some());
        for thread in came_to_need.into_iter().filter(|_| linked) {
            cx.emit(WorkspaceEvent::Attention(thread));
        }
        if moved {
            self.update_awake(cx);
            self.agents_moved(cx);
        }
        if self.faces.threads.meters != meters_before {
            cx.notify();
        }
        cx.notify();
    }

    /// Whether any agent works, by its worker's own table.
    pub(super) fn any_working(&self) -> bool {
        self.faces.threads.stands.values().any(|s| s.rung == Rung::Working)
    }

    /// The server's ladder: where the threads of every worker stand, for those whose own link
    /// is down or was never up.
    pub fn server_ladder(&mut self, ladder: &Ladder, cx: &mut Context<Self>) {
        let stands: HashMap<ThreadId, ThreadStand> = ladder
            .threads
            .iter()
            .map(|r| {
                let stand = ThreadStand {
                    worker: super::keys::worker_key(r.at.worker),
                    rung: r.rung,
                    asks: None,
                    since: r.since_ms,
                    status: None,
                    doing: None,
                    // What is unread is its worker's table's word, heard only over a link.
                    ended: None,
                    seen: TurnId::BEFORE,
                };
                (r.at.thread, stand)
            })
            .collect();
        if stands != self.faces.threads.server {
            // One that comes to need the person, on a worker no link of this client's would
            // say it from, sounds here.
            let linked = |key: WorkerKey| self.workers.get(&key).is_some_and(|w| w.link.is_some());
            let old = &self.faces.threads.server;
            let came: Vec<ThreadId> = stands
                .iter()
                .filter(|(_, st)| st.rung == Rung::NeedsYou && !linked(st.worker))
                .filter(|(id, _)| old.get(*id).is_some_and(|was| was.rung != Rung::NeedsYou))
                .map(|(id, _)| *id)
                .collect();
            self.faces.threads.server = stands;
            for thread in came {
                cx.emit(WorkspaceEvent::Attention(thread));
            }
            self.agents_moved(cx);
            cx.notify();
        }
    }

    /// Drop what the server's ladder said: on `worker` (gone), or everywhere (`None`, the
    /// server was disconnected). Whether anything went.
    pub(super) fn forget_server_threads(&mut self, worker: Option<WorkerKey>) -> bool {
        let before = self.faces.threads.server.len();
        self.faces
            .threads
            .server
            .retain(|_, stand| worker.is_some_and(|gone| stand.worker != gone));
        self.faces.threads.server.len() != before
    }

    /// Where `thread` stands: its worker's own table while that worker's link is up, else
    /// the server's ladder.
    pub(super) fn thread_stand(&self, thread: ThreadId) -> Option<&ThreadStand> {
        let linked = |key: WorkerKey| self.workers.get(&key).is_some_and(|w| w.link.is_some());
        let threads = &self.faces.threads;
        match threads.stands.get(&thread) {
            Some(own) if linked(own.worker) => Some(own),
            _ => threads.server.get(&thread).filter(|s| !linked(s.worker)),
        }
    }

    /// `key`'s threads as its own table last said, whichever speaks for them: what is unread
    /// is the table's word ([`super::turns`]).
    pub(super) fn table_stands(
        &self,
        key: WorkerKey,
    ) -> impl Iterator<Item = (ThreadId, &ThreadStand)> {
        self.faces.threads.stands.iter().filter(move |(_, s)| s.worker == key).map(|(t, s)| (*t, s))
    }

    /// Every thread whose row speaks for it ([`Self::thread_stand`]), on every worker, each
    /// once however many ways its worker is reached.
    pub(super) fn thread_stands(&self) -> impl Iterator<Item = (ThreadId, &ThreadStand)> {
        let threads = &self.faces.threads;
        let only_server = threads.server.keys().filter(|t| !threads.stands.contains_key(t));
        threads.stands.keys().chain(only_server).filter_map(|t| Some((*t, self.thread_stand(*t)?)))
    }

    /// The threads waiting on the person, by worker.
    pub(super) fn threads_waiting(&self) -> Vec<ThreadWait> {
        self.threads_on(Rung::NeedsYou)
    }

    /// The threads on `rung` whose rows speak for them, by worker.
    pub(super) fn threads_on(&self, rung: Rung) -> Vec<ThreadWait> {
        let mut out: Vec<ThreadWait> = self
            .thread_stands()
            .filter(|(_, stand)| stand.rung == rung)
            .map(|(thread, stand)| ThreadWait { worker: stand.worker, thread })
            .collect();
        out.sort_by_key(|w| (w.worker, w.thread));
        out
    }
}

/// Where a thread stands as its worker's table row says it, for the header's pill and the icon
/// badge.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct ThreadStand {
    /// The worker whose agent runs it.
    pub worker: WorkerKey,
    /// Its rung on the attention ladder, its subagents' folded in.
    pub rung: Rung,
    /// What it asks while it waits on the person: its first open request.
    pub asks: Option<RequestCard>,
    /// When its row last changed, by its worker's clock.
    pub since: slopty_core::WallMs,
    /// Where its own agent is, as its adapter maps it (its subagents' not folded in), and
    /// what it waits on: its worker's table says it; the server's ladder ranks only.
    pub status: Option<thread::Status>,
    /// What it is doing now: its newest call's title ("Edit src/main.rs").
    pub doing: Option<String>,
    /// Its latest turn that ended, as its worker's table says; none while one is under way.
    pub ended: Option<thread::wire::TurnEnded>,
    /// The last turn the person has seen, on any device: its worker's mark.
    pub seen: TurnId,
}

/// A thread that waits on the person, for the bell, *Needs you* and the Dock's count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct ThreadWait {
    /// The worker whose agent runs it.
    pub worker: WorkerKey,
    /// The thread.
    pub thread: ThreadId,
}

/// What a thread tile's header says before its worker's table names the thread or its agent.
const THREAD: &str = "Thread";

/// Where each of `key`'s threads stands, from its table: a subagent's rung folded into the
/// thread it hangs from, as the server's ladder folds it, and no row of its own.
fn stands_of<'a>(
    key: WorkerKey,
    rows: impl Iterator<Item = &'a ThreadRow> + Clone,
) -> HashMap<ThreadId, ThreadStand> {
    let parents: HashMap<ThreadId, ThreadId> =
        rows.clone().filter_map(|r| Some((r.id, r.parent.as_ref()?.thread))).collect();
    let top = |mut thread: ThreadId| {
        let mut hops = 0_usize;
        while let Some(parent) = parents.get(&thread).copied() {
            thread = parent;
            hops = hops.saturating_add(1);
            if hops > parents.len() {
                break;
            }
        }
        thread
    };
    let mut out: HashMap<ThreadId, ThreadStand> = rows
        .clone()
        .filter(|r| r.parent.is_none())
        .map(|r| {
            let stand = ThreadStand {
                worker: key,
                rung: Rung::of(r),
                asks: r.requests.first().cloned(),
                since: r.updated_ms,
                status: Some(r.status.clone()),
                doing: r.doing.clone(),
                ended: r.ended,
                seen: r.seen,
            };
            (r.id, stand)
        })
        .collect();
    for row in rows.filter(|r| r.parent.is_some()) {
        if let Some(stand) = out.get_mut(&top(row.id)) {
            let rung = Rung::of(row);
            if rung > stand.rung {
                stand.rung = rung;
                stand.since = row.updated_ms;
            }
            if stand.asks.is_none() {
                stand.asks = row.requests.first().cloned();
            }
        }
    }
    out
}

// slopty: an agent's plan windows on the Agents page.
#[cfg(test)]
mod usage_tests {
    use slopty_client::WorkerKey;
    use slopty_core::WallMs;
    use slopty_proto::thread::Limit;

    use super::ThreadFaces;

    fn limit(name: &str, used_bp: u32) -> Limit {
        Limit { name: name.to_owned(), used_bp, resets_ms: Some(WallMs::from_millis(1_000_000)) }
    }

    /// Two threads of one agent said their windows: the newer reading is the agent's, on one
    /// machine or across two, and another agent's says nothing of it.
    #[test]
    fn the_newer_meter_of_an_agents_threads_wins() {
        let (studio, laptop) = (WorkerKey::new(1), WorkerKey::new(2));
        let mut faces = ThreadFaces::default();
        let now = WallMs::from_millis(5_000);
        assert!(faces.usage_of("claude-code", now).is_none(), "nothing heard, nothing shown");
        faces.meters.hear(
            studio,
            "claude-code",
            &[limit("five-hour", 2_000)],
            WallMs::from_millis(300),
        );
        // A second thread on the same machine, said earlier: the newer word stays.
        faces.meters.hear(
            studio,
            "claude-code",
            &[limit("five-hour", 9_000)],
            WallMs::from_millis(100),
        );
        faces.meters.hear(laptop, "codex", &[limit("five-hour", 7_000)], WallMs::from_millis(900));
        let usage = faces.usage_of("claude-code", now).expect("heard");
        assert_eq!(usage.windows.len(), 1);
        assert!((usage.windows[0].used_fraction - 0.2).abs() < 1e-6);
        // Another machine's thread of it says more, later: that reading is the agent's now.
        faces.meters.hear(
            laptop,
            "claude-code",
            &[limit("seven-day", 4_000)],
            WallMs::from_millis(800),
        );
        let usage = faces.usage_of("claude-code", now).expect("heard");
        assert_eq!(
            usage.windows[0].label,
            crate::view_model::rows::limits(&[limit("seven-day", 4_000)])[0].label
        );
        assert_eq!(usage.fetched_at, Some(crate::view_model::rows::time(WallMs::from_millis(800))));
    }
}
