//! The thread face of an agent's terminal, and threads as tiles of their own.
//!
//! Every worker's threads come through one [`ThreadHub`] per worker, fed by the link
//! ([`WorkspaceView::threads_linked`], [`WorkspaceView::thread_table`],
//! [`WorkspaceView::thread_frame`], [`WorkspaceView::thread_done`]).
//!
//! A terminal whose agent the worker's thread table names can show its TUI or its thread
//! ([`ThreadView`]), toggled per tile (⌘J, the header's button); it opens on the thread on
//! every device. The same PTY and session go on under both, and the pick is saved with the
//! layout. The thread view is made while it shows: its thread is followed while some view of
//! it is open.
//!
//! A thread is a tile of its own too (`ItemKind::Thread`): the thread view, on the worker whose
//! agent runs it, started from the palette ([`WorkspaceView::start_thread`]) or kept from before.
//! A thread whose agent runs in a terminal (Claude Code, pi's TUI) keeps that terminal one action
//! away: the view's "show the terminal" opens its tile beside the thread's, or goes to it.
//!
//! An aside ([`hub::is_aside`]) is its thread view's own business: it is no tile, no row and no
//! terminal's thread here.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{App, AppContext as _, Context, Entity, Focusable as _, Window};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_proto::agent::{AgentKind, AgentStatus};
use slopty_proto::git::GitOutcome;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::thread::attention::{Ladder, Rung};
use slopty_proto::thread::wire::{
    Authors, IntentDone, Outcome, RequestCard, Start, TableFrame, ThreadFrame, ThreadHits,
    ThreadRequest, ThreadRow,
};
use slopty_proto::thread::{AgentId, IntentId, ThreadId, TurnId};
use slopty_proto::{ClientMsg, RequestId};

use super::WorkspaceView;
use super::actions::ToggleConversation;
use crate::conversation::attach::Target;
use crate::conversation::thread::{HubEvent, ThreadHub, ThreadView, ThreadViewEvent, hub};
use crate::icons::Status;
use crate::review::ReviewView;

/// What the workspace keeps about faces.
#[derive(Default)]
pub(super) struct Faces {
    /// The thread or the TUI, as the person last picked for a session.
    pub chosen: HashMap<SessionId, bool>,
    /// Sessions whose thread view takes the keyboard on the next frame.
    pub focus: HashSet<SessionId>,
    /// What each hidden thread view's composer held, put back when its tile shows the thread
    /// again: a view is made only while it shows.
    pub drafts: HashMap<SessionId, String>,
    /// The thread views and the hubs they read.
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
}

/// Each worker's threads, and the thread view of each agent tile that shows one.
#[derive(Default)]
pub(super) struct ThreadFaces {
    /// Each worker's threads, made the first time its link comes up.
    hubs: HashMap<WorkerKey, Entity<ThreadHub>>,
    hearing: HashMap<WorkerKey, gpui::Subscription>,
    /// The thread each agent terminal runs, as its worker's table says.
    of_session: HashMap<SessionId, ThreadId>,
    /// The thread view of each tile that shows one, kept while it shows.
    views: HashMap<SessionId, Entity<ThreadView>>,
    asks: HashMap<SessionId, gpui::Subscription>,
    /// Where each worker's threads are kept: a directory per worker under it.
    cache: Option<PathBuf>,
    /// The review tile of each thread whose review was asked for, kept while it is open.
    reviews: HashMap<ThreadId, Entity<ReviewView>>,
    /// A thread view asked for its thread's review: made in the next sync, for the strip to
    /// open ([`WorkspaceView::take_review`]).
    review_asked: Option<(WorkerKey, ThreadId)>,
    /// A review tile made and not yet opened, and the worker whose agent runs its thread.
    review_made: Option<(WorkerKey, ThreadId)>,
    /// The thread view of each thread tile, kept while its tile is there.
    items: HashMap<ItemId, Entity<ThreadView>>,
    item_asks: HashMap<ItemId, gpui::Subscription>,
    /// Each thread's title as its worker's table last said, for its tile's header.
    titles: HashMap<ThreadId, String>,
    /// The last line each thread's agent wrote as its worker's table last said, for the
    /// navigator, the overview and the palette.
    lines: HashMap<ThreadId, String>,
    /// Each thread's agent as its worker's table last said, for the mark its tile leads with.
    agents: HashMap<ThreadId, AgentId>,
    /// Each thread's terminal as its worker's table last said, for its tile's way to it.
    terminals: HashMap<ThreadId, SessionId>,
    /// Each thread's open facts as its worker's table last said (its project, its task), for
    /// the group its tile and its terminal's are in.
    facts: HashMap<ThreadId, std::collections::BTreeMap<String, String>>,
    /// Where each thread works as its worker's table last said (its directory, the repository
    /// that is in), for the project of a thread tile with no terminal.
    places: HashMap<ThreadId, ThreadPlace>,
    /// Where each thread stands as its worker's table last said, its subagents folded in, for
    /// the chrome outside its tile.
    stands: HashMap<ThreadId, ThreadStand>,
    /// Where each thread stands as the server's ladder last said: what speaks for the threads
    /// of a worker this client has no link to.
    server: HashMap<ThreadId, ThreadStand>,
    /// Each machine's plan windows as its agents' rows last said them, for the status bar.
    meters: slopty_client::meters::PlanMeters,
    /// Starts sent and not yet answered: the worker each went to, its agent, and the tile it
    /// fills.
    starts: HashMap<IntentId, (WorkerKey, AgentId, ItemId)>,
    /// The thread tile whose view takes the keyboard once it is made.
    focus_item: Option<ItemId>,
    /// Threads opened at a turn, and the turn, until a view of each shows it.
    going: HashMap<ThreadId, TurnId>,
}

impl ThreadFaces {
    /// Each machine's plan windows, for the status bar.
    pub(super) const fn meters(&self) -> &slopty_client::meters::PlanMeters {
        &self.meters
    }

    /// Forget a machine's plan windows, as it goes from the workspace.
    pub(super) fn forget_meters(&mut self, worker: WorkerKey) {
        self.meters.forget(worker);
    }
}

impl WorkspaceView {
    /// Whether `session`'s tile shows its agent's thread: the person's pick, else the thread.
    /// Only while an agent runs in it and its worker's table names its thread.
    #[must_use]
    pub fn face_shown(&self, session: SessionId) -> bool {
        let agent = self.agent_state(session).is_some_and(|a| a.status != AgentStatus::None);
        agent
            && self.faces.threads.of_session.contains_key(&session)
            && self.faces.chosen.get(&session).copied().unwrap_or(true)
    }

    /// The thread view `session`'s tile shows in place of its TUI, once made.
    #[must_use]
    pub fn thread_face(&self, session: SessionId) -> Option<&Entity<ThreadView>> {
        self.faces.threads.views.get(&session).filter(|_| self.face_shown(session))
    }

    /// The review tile a thread view asked for since this was last asked, with the worker
    /// whose agent runs its thread, for the strip to open as a tile there (`Handed::Review`).
    pub fn take_review(&mut self) -> Option<(WorkerKey, ThreadId, Entity<ReviewView>)> {
        let (key, thread) = self.faces.threads.review_made.take()?;
        self.faces.threads.reviews.get(&thread).map(|view| (key, thread, view.clone()))
    }

    /// The review tile of `thread` on `key`, made when there is none yet: a review item the
    /// registry holds with no view, as after a relaunch, gets its view here.
    pub fn open_review(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ReviewView> {
        if let Some(view) = self.faces.threads.reviews.get(&thread) {
            return view.clone();
        }
        let hub = self.thread_hub(key, cx);
        let theme = self.theme.clone();
        let view = cx.new(|cx| ReviewView::new(hub, thread, theme, window, cx));
        self.faces.threads.reviews.insert(thread, view.clone());
        view
    }

    /// Every review tile open.
    pub(super) fn open_reviews(&self) -> impl Iterator<Item = &Entity<ReviewView>> {
        self.faces.threads.reviews.values()
    }

    /// The review tile of `thread`, while one is open.
    #[must_use]
    pub fn review_of(&self, thread: ThreadId) -> Option<&Entity<ReviewView>> {
        self.faces.threads.reviews.get(&thread)
    }

    /// The strip closed `thread`'s review tile: the thread is no longer followed for it.
    pub fn review_closed(&mut self, thread: ThreadId) {
        self.faces.threads.reviews.remove(&thread);
    }

    /// Draw every thread view and review tile in `theme`.
    pub(super) fn set_threads_theme(&self, theme: &slopty_theme::Theme, cx: &mut Context<Self>) {
        for view in self.faces.threads.views.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.faces.threads.reviews.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.faces.threads.items.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
    }

    /// Start a thread of `agent` on `key`, in `cwd`, with `prompt` as its first word or nothing
    /// said yet: its tile opens at once saying it is starting, and the worker answers with the
    /// thread, which takes the tile ([`Self::thread_done`]), or in words why not.
    pub fn start_thread(
        &mut self,
        key: WorkerKey,
        agent: AgentId,
        cwd: String,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let item = ItemId::new();
        self.open_starting(item, super::starting::Starting::new(key, agent, cwd, None), cx);
        self.send_start(item, prompt, cx);
    }

    /// Take a past session of `agent` on `key` up again by a start in `cwd` with `args`, the
    /// agent's own words for it: its tile opens at once saying it is starting.
    pub(super) fn start_resumed(
        &mut self,
        key: WorkerKey,
        agent: AgentId,
        cwd: String,
        args: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let item = ItemId::new();
        let starting = super::starting::Starting::new(key, agent, cwd, None).with_args(args);
        self.open_starting(item, starting, cx);
        self.send_start(item, None, cx);
    }

    /// Send the start of the thread on its way in `item`'s tile, with `prompt` as its first
    /// message.
    pub(super) fn send_start(
        &mut self,
        item: ItemId,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(starting) = self.starting.get(item) else { return };
        let key = starting.worker;
        let id = IntentId::new();
        let start = Start {
            agent: starting.agent.clone(),
            cwd: starting.cwd.clone(),
            drive: None,
            prompt,
            model: None,
            args: starting.agent_args(),
            worktree: starting.worktree.clone(),
        };
        tracing::info!(%key, %id, %item, agent = %start.agent.0, cwd = start.cwd, "start thread");
        self.faces.threads.starts.insert(id, (key, start.agent.clone(), item));
        self.send(key, ClientMsg::Thread(ThreadRequest::Start { id, start: Box::new(start) }));
        self.starting_sent(item, cx);
    }

    /// The thread view a thread tile shows, once made.
    #[must_use]
    pub fn thread_item(&self, item: ItemId) -> Option<&Entity<ThreadView>> {
        self.faces.threads.items.get(&item)
    }

    /// A thread's name, the one every surface calls it by: a thread tile's header, the
    /// navigator, the inbox, and who wrote a line. Where its agent runs in a tile here, the
    /// tile's title (the name the person gave it, its project role, or what the agent titled
    /// itself); else its title as its worker's table says it.
    #[must_use]
    pub fn thread_title(&self, thread: ThreadId) -> String {
        let tile = self.thread_terminal(thread).and_then(|session| {
            self.items().find_map(|(_, item)| match item.kind {
                ItemKind::Terminal { session: s } if s == session => Some(self.tile_title(item)),
                _ => None,
            })
        });
        tile.or_else(|| self.thread_named(thread)).unwrap_or_else(|| THREAD.to_owned())
    }

    /// The agent `item` shows, by its [`AgentId`] name: a thread's, or the one at work in a
    /// terminal, as its thread's row names it or else by the kind the terminal reports.
    pub(super) fn item_agent(&self, item: &Item) -> Option<&str> {
        match item.kind {
            ItemKind::Thread { thread } => self.thread_agent(thread),
            ItemKind::Terminal { session } => self.session_agent(session),
            _ => None,
        }
    }

    /// The agent at work in `session`, by its [`AgentId`] name: as its thread's row names it,
    /// or else by the kind the terminal reports.
    pub(super) fn session_agent(&self, session: SessionId) -> Option<&str> {
        let threads = &self.faces.threads;
        let at_work = self.agent_state(session).filter(|a| a.status != AgentStatus::None)?;
        let named = threads.of_session.get(&session).and_then(|t| threads.agents.get(t));
        Some(named.map_or_else(
            || match at_work.kind {
                AgentKind::ClaudeCode => AgentId::CLAUDE_CODE,
            },
            |a| a.0.as_str(),
        ))
    }

    /// `thread`'s open facts, as its worker's table last said.
    pub(super) fn thread_facts(
        &self,
        thread: ThreadId,
    ) -> Option<&std::collections::BTreeMap<String, String>> {
        self.faces.threads.facts.get(&thread)
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

    /// The terminal `thread`'s TUI runs in, as its worker's table last said.
    pub(super) fn thread_terminal(&self, thread: ThreadId) -> Option<SessionId> {
        self.faces.threads.terminals.get(&thread).copied()
    }

    /// The thread `session`'s agent runs, as its worker's table says.
    pub(super) fn session_thread(&self, session: SessionId) -> Option<ThreadId> {
        self.faces.threads.of_session.get(&session).copied()
    }

    /// The agent that runs `thread`, by its [`AgentId`] name, as its worker's table last said.
    pub(super) fn thread_agent(&self, thread: ThreadId) -> Option<&str> {
        self.faces.threads.agents.get(&thread).map(|a| a.0.as_str())
    }

    /// What `item`'s tile leads with ([`super::tile::kind_icon`]), its agent looked up.
    pub(super) fn kind_glyph(&self, item: &Item) -> crate::icons::Glyph {
        super::tile::kind_icon(item, self.item_agent(item))
    }

    /// Keep each worker's threads in `dir`, a directory per worker.
    pub fn set_thread_cache(&mut self, dir: PathBuf) {
        self.faces.threads.cache = Some(dir);
    }

    /// `key`'s threads, once they have been made.
    pub(super) fn held_hub(&self, key: WorkerKey) -> Option<&Entity<ThreadHub>> {
        self.faces.threads.hubs.get(&key)
    }

    /// `thread` as a line's author: its agent, and its name ([`Self::thread_title`]).
    pub(super) fn writer_of(&self, thread: ThreadId) -> Option<crate::authorship::Writer> {
        let agent = self.faces.threads.agents.get(&thread)?.clone();
        Some(crate::authorship::Writer { agent, title: self.thread_title(thread) })
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

    /// Show `thread` at `turn` once a view of it is there.
    pub(super) fn go_to_turn_when_shown(&mut self, thread: ThreadId, turn: TurnId) {
        self.faces.threads.going.insert(thread, turn);
    }

    /// Each view of a thread opened at a turn goes to it; the thread is let go of once one has.
    pub(super) fn settle_going(&mut self, cx: &mut Context<Self>) {
        if self.faces.threads.going.is_empty() {
            return;
        }
        let views: Vec<Entity<ThreadView>> = self
            .faces
            .threads
            .items
            .values()
            .chain(self.faces.threads.views.values())
            .cloned()
            .collect();
        for view in views {
            let thread = view.read(cx).thread();
            if let Some(turn) = self.faces.threads.going.remove(&thread) {
                view.update(cx, |v, cx| v.go_to_turn(turn, cx));
            }
        }
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
            HubEvent::Table => this.threads_of_sessions(key, cx),
            HubEvent::Authors => this.author_files(key, cx),
            // An aside's fork stays in the sheet of the view that asked it.
            HubEvent::Started { thread, aside: false, .. } => this.open_thread(key, *thread, cx),
            _ => {}
        });
        self.faces.threads.hearing.insert(key, hearing);
        self.faces.threads.hubs.insert(key, hub.clone());
        self.hub_agents(key, cx);
        hub
    }

    /// What `key` found in its threads for the words its hub last asked.
    pub fn thread_hits(&mut self, key: WorkerKey, hits: ThreadHits, cx: &mut Context<Self>) {
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.thread_hits(hits, cx));
    }

    /// `key` said who wrote the lines of a file a tile shows.
    pub fn thread_authors(&mut self, key: WorkerKey, authors: Authors, cx: &mut Context<Self>) {
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.heard_authors(authors, cx));
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

    /// The link to `key` is up: its threads catch up from where they stand.
    pub fn threads_linked(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let hub = self.thread_hub(key, cx);
        hub.update(cx, ThreadHub::connected);
    }

    /// Tell `key`'s thread hub what that machine can start now ("Continue in…"), if it has
    /// one: when the hub is made, and when the machine's caps or link move.
    pub(super) fn hub_agents(&self, key: WorkerKey, cx: &mut Context<Self>) {
        if let Some(hub) = self.faces.threads.hubs.get(&key).cloned() {
            let agents = self.startable_on(key);
            hub.update(cx, |hub, cx| hub.set_agents(agents, cx));
        }
    }

    /// What `key`'s thread hub was told it can start.
    #[cfg(test)]
    pub(super) fn hub_agents_of(&self, key: WorkerKey, cx: &App) -> Option<Vec<AgentId>> {
        Some(self.faces.threads.hubs.get(&key)?.read(cx).agents().to_vec())
    }

    /// The link to `key` went: its thread views show what they last knew.
    pub fn threads_unlinked(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        if let Some(hub) = self.faces.threads.hubs.get(&key) {
            hub.update(cx, ThreadHub::disconnected);
        }
        self.faces.threads.starts.retain(|_, (at, ..)| *at != key);
        self.starts_unlinked(key, cx);
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
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.frame(thread, frame, cx));
    }

    /// `key`'s answer to one of this client's intents. A start answered with its thread
    /// opens the thread as a tile there; one refused says why.
    pub fn thread_done(&mut self, key: WorkerKey, done: &IntentDone, cx: &mut Context<Self>) {
        if self.faces.threads.starts.get(&done.id).is_some_and(|(at, ..)| *at == key) {
            let Some((_, agent, item)) = self.faces.threads.starts.remove(&done.id) else {
                return;
            };
            match &done.outcome {
                Outcome::Started { thread } => self.start_landed(key, item, *thread, &agent, cx),
                Outcome::Refused { reason } => self.start_failed(key, item, reason.clone(), cx),
                Outcome::Unsupported { .. } | Outcome::Done | Outcome::Accepted => {
                    let text = format!(
                        "{} can\u{2019}t start {}",
                        self.worker_name(key),
                        super::projects::agent_label(&agent)
                    );
                    self.start_failed(key, item, text, cx);
                }
            }
            return;
        }
        if let Some(hub) = self.faces.threads.hubs.get(&key) {
            hub.update(cx, |hub, cx| hub.done(done, cx));
        }
    }

    /// Go to `thread`'s tile, or add one on `key`.
    pub(super) fn open_thread(&mut self, key: WorkerKey, thread: ThreadId, cx: &mut Context<Self>) {
        if let Some(tile) = self.tile_of_thread(thread) {
            self.go_to(tile.item, cx);
            return;
        }
        self.open_thread_as(key, thread, ItemId::new(), cx);
    }

    /// Add `thread`'s tile on `key` as item `id`: a start's tile keeps its place under it.
    pub(super) fn open_thread_as(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        id: ItemId,
        cx: &mut Context<Self>,
    ) {
        let item = Item {
            id,
            kind: ItemKind::Thread { thread },
            name: None,
            facts: std::collections::BTreeMap::new(),
        };
        tracing::info!(id = %item.id, %thread, "open thread");
        self.propose(key, ItemOp::Add(item), cx);
        cx.notify();
    }

    /// A closed agent tile taken back after its session ended: `thread`'s tile where it was
    /// (`at`), focused, and its agent taken up again through its own door once the thread is
    /// known here ([`ThreadHub::resume_when_known`]).
    pub(super) fn reopen_thread(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        at: Option<slopty_client::layout::Pos>,
        cx: &mut Context<Self>,
    ) {
        let tile = self.tile_of_thread(thread).unwrap_or_else(|| {
            let id = ItemId::new();
            self.open_thread_as(key, thread, id, cx);
            TileRef { worker: key, item: id }
        });
        if let Some(at) = at {
            self.tick();
            self.layout.move_tile(
                tile,
                slopty_client::layout::DropTarget::NewColumn {
                    workspace: at.workspace,
                    index: at.column,
                },
            );
            self.layout_touched(cx);
        }
        self.focus_tile(tile, cx);
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.resume_when_known(thread, cx));
    }

    /// The tile that shows `thread`, on any worker.
    pub(super) fn tile_of_thread(&self, thread: ThreadId) -> Option<TileRef> {
        self.items().find_map(|(worker, item)| match item.kind {
            ItemKind::Thread { thread: t } if t == thread => {
                Some(TileRef { worker, item: item.id })
            }
            _ => None,
        })
    }

    /// Go to the tile of `thread`'s terminal on `key`, or add one beside the thread's.
    fn open_thread_terminal(&mut self, key: WorkerKey, thread: ThreadId, cx: &mut Context<Self>) {
        let Some(session) = self.faces.threads.terminals.get(&thread).copied() else {
            self.show_notice("This thread runs in no terminal".to_owned(), cx);
            return;
        };
        if self.tile_of_session(session).is_some() {
            self.reveal_session(session, cx);
            return;
        }
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: std::collections::BTreeMap::new(),
        };
        self.propose(key, ItemOp::Add(item), cx);
        self.pending_focus = Some(session);
        cx.notify();
    }

    /// Which thread each of `key`'s terminals runs, from its table: a subagent's thread is
    /// its parent's business, not a tile's, and an aside its asker's.
    fn threads_of_sessions(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let Some(hub) = self.faces.threads.hubs.get(&key) else { return };
        let rows: Vec<&ThreadRow> =
            hub.read(cx).threads().rows().rows.values().filter(|r| !hub::is_aside(r)).collect();
        let found: Vec<(SessionId, ThreadId)> = rows
            .iter()
            .filter(|row| row.parent.is_none())
            .filter_map(|row| Some((row.terminal?, row.id)))
            .collect();
        let sessions: HashSet<SessionId> = self
            .workers
            .get(&key)
            .map(|w| w.sessions.keys().copied().collect())
            .unwrap_or_default();
        let of = &mut self.faces.threads.of_session;
        of.retain(|session, _| !sessions.contains(session));
        of.extend(found);
        let stands = stands_of(key, rows.iter().copied());
        let old = &self.faces.threads.stands;
        let moved = old.values().filter(|s| s.worker == key).count() != stands.len()
            || stands.iter().any(|(id, stand)| old.get(id) != Some(stand));
        // A thread heard before that comes to need the person: the corner may point at it.
        let came_to_need: Vec<ThreadId> = stands
            .iter()
            .filter(|(id, stand)| {
                stand.rung == Rung::NeedsYou
                    && old.get(*id).is_some_and(|was| was.rung != Rung::NeedsYou)
            })
            .map(|(id, _)| *id)
            .collect();
        self.faces.threads.stands.retain(|_, stand| stand.worker != key);
        self.faces.threads.stands.extend(stands);
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
            };
            self.faces.threads.places.insert(row.id, place);
            self.faces.threads.meters.hear(key, &row.agent.0, &row.meters.limits, row.updated_ms);
            match row.terminal {
                Some(session) => self.faces.threads.terminals.insert(row.id, session),
                None => self.faces.threads.terminals.remove(&row.id),
            };
        }
        for thread in came_to_need {
            let Some(word) = self.thread_stand(thread).and_then(ThreadStand::word) else {
                continue;
            };
            if let Some(tile) = self.tile_of_thread(thread) {
                self.attention_toast(tile, Status::NeedsYou, &word.to_lowercase(), cx);
            }
        }
        if moved {
            self.agents_moved(cx);
        }
        // A note's verdict that waited for its request answers it as the table brings it.
        if self.approvals.taps_waiting() {
            self.settle_taps(cx);
        }
        if self.faces.threads.meters != meters_before {
            App::notify(cx, self.chrome.statusbar.entity_id());
        }
        cx.notify();
    }

    /// Make the thread views the shown tiles want, and let go of those no tile shows: the
    /// last view of a thread unfollows it.
    fn sync_thread_faces(
        &mut self,
        wanted: &[SessionId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for session in wanted {
            let Some(thread) = self.faces.threads.of_session.get(session).copied() else {
                continue;
            };
            let current = self.faces.threads.views.get(session).map(|v| v.read(cx).thread());
            if current == Some(thread) {
                continue;
            }
            let Some(key) = self.worker_of_session(*session) else { continue };
            let hub = self.thread_hub(key, cx);
            let theme = self.theme.clone();
            let draft = self.faces.drafts.remove(session);
            let view = cx.new(|cx| {
                let mut view = ThreadView::new(hub, thread, theme, window, cx);
                if let Some(draft) = draft {
                    view.restore_draft(&draft, window, cx);
                }
                view
            });
            let session = *session;
            // The keyboard follows the tile into the new view from what it replaces: the
            // thread's last view, or the TUI the tile showed until its thread was known.
            let replaced =
                self.faces.threads.views.get(&session).map(|v| v.read(cx).focus_handle(cx));
            let terminal = self.terminals.get(&session).map(|t| t.read(cx).focus_handle(cx));
            if replaced.into_iter().chain(terminal).any(|h| h.contains_focused(window, cx)) {
                self.faces.focus.insert(session);
            }
            let asks = cx.subscribe(&view, move |this, view, event: &ThreadViewEvent, cx| {
                this.thread_view_event(session, &view, event.clone(), cx);
            });
            self.faces.threads.asks.insert(session, asks);
            self.faces.threads.views.insert(session, view);
        }
        if let Some((key, thread)) = self.faces.threads.review_asked.take() {
            let _view = self.open_review(key, thread, window, cx);
            self.faces.threads.review_made = Some((key, thread));
            cx.notify();
        }
        // A view that goes while it holds the keyboard hands it back to its tile, which then
        // shows its TUI or its thread's next view: a thread that moved to another terminal or
        // ended must not leave the keyboard with nothing.
        let threads = &self.faces.threads;
        let held = threads.views.iter().find_map(|(s, view)| {
            let gone = !wanted.contains(s) || !threads.of_session.contains_key(s);
            (gone && view.read(cx).focus_handle(cx).contains_focused(window, cx)).then_some(*s)
        });
        if held.is_some() {
            self.pending_focus = held;
        }
        let threads = &mut self.faces.threads;
        let drafts = &mut self.faces.drafts;
        threads.views.retain(|s, view| {
            let kept = wanted.contains(s) && threads.of_session.contains_key(s);
            let draft = view.read(cx).draft(cx);
            if !kept && !draft.is_empty() {
                drafts.insert(*s, draft);
            }
            kept
        });
        let views = &threads.views;
        threads.asks.retain(|s, _| views.contains_key(s));
    }

    /// Make the thread view of each thread tile, and let go of those whose tile is gone. A view
    /// made for the focused tile takes the keyboard.
    fn sync_thread_items(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tiled: Vec<(ItemId, WorkerKey, ThreadId)> = self
            .layout
            .tiles()
            .filter_map(|t| match self.item(t)?.kind {
                ItemKind::Thread { thread } => Some((t.item, t.worker, thread)),
                _ => None,
            })
            .collect();
        for &(item, key, thread) in &tiled {
            if self.faces.threads.items.contains_key(&item) {
                continue;
            }
            let hub = self.thread_hub(key, cx);
            let theme = self.theme.clone();
            let view = cx.new(|cx| ThreadView::new(hub, thread, theme, window, cx));
            let asks = cx.subscribe(&view, move |this, view, event: &ThreadViewEvent, cx| {
                this.thread_item_event(key, item, &view, event.clone(), cx);
            });
            self.faces.threads.item_asks.insert(item, asks);
            self.faces.threads.items.insert(item, view);
        }
        let threads = &mut self.faces.threads;
        threads.items.retain(|item, _| tiled.iter().any(|(i, ..)| i == item));
        let items = &threads.items;
        threads.item_asks.retain(|item, _| items.contains_key(item));
        let focus = threads.focus_item.take().and_then(|item| threads.items.get(&item)).cloned();
        if let Some(view) = focus {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
    }

    /// The thread tile `item` took the focus: its view takes the keyboard in the next frame,
    /// made first when it is new.
    pub(super) fn focus_thread_item(&mut self, item: ItemId, cx: &mut Context<Self>) {
        self.faces.threads.focus_item = Some(item);
        self.faces_dirty = true;
        cx.notify();
    }

    /// What the view of thread tile `item` asks for. Files attached go up to the tile's worker.
    fn thread_item_event(
        &mut self,
        key: WorkerKey,
        item: ItemId,
        view: &Entity<ThreadView>,
        event: ThreadViewEvent,
        cx: &mut Context<Self>,
    ) {
        let thread = view.read(cx).thread();
        match event {
            ThreadViewEvent::ShowTerminal => self.open_thread_terminal(key, thread, cx),
            ThreadViewEvent::Review { thread } => {
                self.faces.threads.review_asked = Some((key, thread));
                cx.notify();
            }
            ThreadViewEvent::Attach { id, what } => {
                let tile = self.tile_of(item);
                self.attach_to_composer(tile, Target(view.downgrade()), id, what, cx);
            }
            ThreadViewEvent::Detach { id } => {
                self.detach_from_composer(&Target(view.downgrade()), id, cx);
            }
            ThreadViewEvent::Watch { thread, screen } => {
                self.watch_agent_screen(key, thread, &screen, cx);
            }
            ThreadViewEvent::PickFiles => {
                if let Some(tile) = self.tile_of(item) {
                    self.ask_files(&super::folders::FilesAsk::Import(tile), cx);
                }
            }
            ThreadViewEvent::FindFiles { root, query } => {
                self.send(key, ClientMsg::FindFiles { root, query });
            }
        }
    }

    /// The subagent threads under `session`'s own thread, however deep: each row whose chain
    /// of parents reaches it.
    pub(super) fn subagents(&self, session: SessionId, cx: &App) -> Vec<ThreadRow> {
        let Some(root) = self.faces.threads.of_session.get(&session).copied() else {
            return Vec::new();
        };
        let Some(hub) =
            self.worker_of_session(session).and_then(|k| self.faces.threads.hubs.get(&k))
        else {
            return Vec::new();
        };
        let rows = &hub.read(cx).threads().rows().rows;
        let under = |row: &ThreadRow| {
            // A chain longer than the table loops: it reaches nothing.
            let mut link = row.parent.as_ref();
            for _ in 0..rows.len() {
                let Some(parent) = link else { return false };
                if parent.thread == root {
                    return true;
                }
                link = rows.get(&parent.thread).and_then(|r| r.parent.as_ref());
            }
            false
        };
        rows.values().filter(|row| under(row)).cloned().collect()
    }

    /// What a thread view asks for.
    fn thread_view_event(
        &mut self,
        session: SessionId,
        view: &Entity<ThreadView>,
        event: ThreadViewEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ThreadViewEvent::ShowTerminal => self.show_face(session, false, cx),
            ThreadViewEvent::Review { thread } => {
                if let Some(key) = self.worker_of_session(session) {
                    self.faces.threads.review_asked = Some((key, thread));
                    cx.notify();
                }
            }
            ThreadViewEvent::Attach { id, what } => {
                let tile = self.tile_of_session(session);
                self.attach_to_composer(tile, Target(view.downgrade()), id, what, cx);
            }
            ThreadViewEvent::Detach { id } => {
                self.detach_from_composer(&Target(view.downgrade()), id, cx);
            }
            ThreadViewEvent::Watch { thread, screen } => {
                if let Some(key) = self.worker_of_session(session) {
                    self.watch_agent_screen(key, thread, &screen, cx);
                }
            }
            ThreadViewEvent::PickFiles => {
                if let Some(tile) = self.tile_of_session(session) {
                    self.ask_files(&super::folders::FilesAsk::Import(tile), cx);
                }
            }
            ThreadViewEvent::FindFiles { root, query } => {
                if let Some(key) = self.worker_of_session(session) {
                    self.send(key, ClientMsg::FindFiles { root, query });
                }
            }
        }
    }

    /// `key` found `paths` under `root` for `query`: the thread views of its tiles that asked
    /// list them in their `@` menus.
    pub fn threads_found(
        &self,
        key: WorkerKey,
        root: &str,
        query: &str,
        paths: &[String],
        cx: &mut Context<Self>,
    ) {
        for (session, view) in &self.faces.threads.views {
            if self.worker_of_session(*session) == Some(key) {
                view.update(cx, |v, cx| v.files_found(root, query, paths, cx));
            }
        }
        for (item, view) in &self.faces.threads.items {
            if self.tile_of(*item).is_some_and(|t| t.worker == key) {
                view.update(cx, |v, cx| v.files_found(root, query, paths, cx));
            }
        }
    }

    /// The composer files dropped on `session`'s tile are attached to: its thread view's
    /// while that shows.
    pub(super) fn shown_composer(&self, session: SessionId) -> Option<Target> {
        self.thread_face(session).map(|view| Target(view.downgrade()))
    }

    /// Whether `session`'s tile shows its thread with a request open in it: the thread's tray
    /// then says what the agent waits on, and nothing else on the tile says it again.
    #[must_use]
    pub fn face_asks(&self, session: SessionId) -> bool {
        self.thread_face(session).is_some() && self.session_request(session).is_some()
    }

    /// Show `session`'s face or its TUI.
    pub fn show_face(&mut self, session: SessionId, face: bool, cx: &mut Context<Self>) {
        self.faces.chosen.insert(session, face);
        // The pick is saved with the layout, by its tile.
        self.layout_touched(cx);
        if face {
            self.faces.focus.insert(session);
        } else {
            self.faces.focus.remove(&session);
            self.pending_focus = Some(session);
        }
        cx.notify();
    }

    /// ⌘J: the focused agent terminal between its TUI and its thread.
    pub(super) fn toggle_conversation(
        &mut self,
        _: &ToggleConversation,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.focused_session() else { return };
        if self.agent_state(session).is_none_or(|a| a.status == AgentStatus::None) {
            self.show_notice("No agent runs in this terminal".to_owned(), cx);
            return;
        }
        if self.session_thread(session).is_none() {
            self.show_notice("This agent has no thread yet".to_owned(), cx);
            return;
        }
        let face = !self.face_shown(session);
        self.show_face(session, face, cx);
    }

    /// The session of the focused tile, if it is a terminal.
    pub(super) fn focused_session(&self) -> Option<SessionId> {
        match self.item(self.focused()?)?.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        }
    }

    /// Bring the thread views in step with the tiles, once a frame: make the view each shown
    /// tile wants and let go of the rest.
    pub(super) fn sync_faces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_faces();
        let wanted: Vec<SessionId> = self
            .layout
            .tiles()
            .filter_map(|t| match self.item(t)?.kind {
                ItemKind::Terminal { session } => Some(session),
                _ => None,
            })
            .filter(|s| self.terminals.contains_key(s) && self.face_shown(*s))
            .collect();
        self.sync_thread_faces(&wanted, window, cx);
        self.sync_thread_items(window, cx);
        // Picks of sessions that are gone go with them.
        let terminals = &self.terminals;
        self.faces.chosen.retain(|s, _| terminals.contains_key(s));
        self.faces.drafts.retain(|s, _| terminals.contains_key(s));
        for session in std::mem::take(&mut self.faces.focus) {
            if !wanted.contains(&session) {
                continue;
            }
            if let Some(view) = self.faces.threads.views.get(&session).cloned() {
                view.update(cx, |v, cx| v.focus(window, cx));
            }
        }
    }

    /// `session`'s agent in the status vocabulary, as a tile marks it: the worker's word, but
    /// never calmer than working while its thread's row says a turn runs. A hook can lag or be
    /// missing; the transcript the thread folds in then knows more, and saying "Idle" over a
    /// spinning row reads as a broken status. Only the mark follows: nothing is driven.
    pub(super) fn agent_mark(&self, session: SessionId) -> Option<Status> {
        let status = self.agent_state(session).and_then(Status::of_agent)?;
        let calm = matches!(status, Status::Idle | Status::Done);
        let working = self.session_stand(session).is_some_and(|st| st.rung == Rung::Working);
        Some(if calm && working { Status::Working } else { status })
    }

    /// Where `session`'s own thread stands as its worker's table says, though its terminal's
    /// agent status speaks for it in the chrome ([`Self::thread_stand`]).
    fn session_stand(&self, session: SessionId) -> Option<&ThreadStand> {
        self.faces.threads.stands.get(&self.session_thread(session)?)
    }

    /// The request `thread` waits on as its worker's table says, while that worker is linked:
    /// whether its terminal's agent or its own row speaks for it.
    pub(super) fn thread_request(&self, thread: ThreadId) -> Option<&RequestCard> {
        let stand = self.faces.threads.stands.get(&thread)?;
        self.workers.get(&stand.worker).and_then(|w| w.link.as_ref())?;
        stand.asks.as_ref()
    }

    /// The request `session`'s agent waits on, as its thread's row says.
    pub(super) fn session_request(&self, session: SessionId) -> Option<&RequestCard> {
        self.thread_request(self.session_thread(session)?)
    }

    /// The server's ladder: where the threads of every worker stand, for those whose own link
    /// is down or was never up, as [`Self::server_agents_replace`] is for their terminals. A
    /// thread names the terminal its TUI runs in, so one whose terminal's agent already speaks
    /// for it is counted once, by the terminal.
    pub fn server_ladder(&mut self, ladder: &Ladder, cx: &mut Context<Self>) {
        let stands: HashMap<ThreadId, ThreadStand> = ladder
            .threads
            .iter()
            .map(|r| {
                let stand = ThreadStand {
                    worker: super::projects::worker_key(r.at.worker),
                    rung: r.rung,
                    // The server's ladder ranks threads; whether an agent exited is its
                    // worker's word.
                    exited: false,
                    asks: None,
                    terminal: r.terminal,
                    since: r.since_ms,
                };
                (r.at.thread, stand)
            })
            .collect();
        if stands != self.faces.threads.server {
            self.faces.threads.server = stands;
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

    /// Where `thread` stands, when its row speaks for it: its worker's own table while that
    /// worker's link is up, else the server's ladder; and not while its terminal's agent
    /// status already speaks for it (Claude Code observed in its TUI), which would count it
    /// twice.
    pub(super) fn thread_stand(&self, thread: ThreadId) -> Option<&ThreadStand> {
        let linked = |key: WorkerKey| self.workers.get(&key).is_some_and(|w| w.link.is_some());
        let threads = &self.faces.threads;
        let stand = match threads.stands.get(&thread) {
            Some(own) if linked(own.worker) => own,
            _ => threads.server.get(&thread).filter(|s| !linked(s.worker))?,
        };
        let spoken = stand
            .terminal
            .and_then(|s| self.agent_state(s))
            .is_some_and(|a| a.status != AgentStatus::None);
        (!spoken).then_some(stand)
    }

    /// Every thread whose row speaks for it ([`Self::thread_stand`]), on every worker, each
    /// once however many ways its worker is reached.
    pub(super) fn thread_stands(&self) -> impl Iterator<Item = (ThreadId, &ThreadStand)> {
        let threads = &self.faces.threads;
        let only_server = threads.server.keys().filter(|t| !threads.stands.contains_key(t));
        threads.stands.keys().chain(only_server).filter_map(|t| Some((*t, self.thread_stand(*t)?)))
    }

    /// The threads waiting on the person whose rows speak for them, after the sessions of
    /// [`Self::needs_you`]: those with a tile in reading order, then the rest by worker.
    pub(super) fn threads_waiting(&self) -> Vec<ThreadWait> {
        self.threads_on(Rung::NeedsYou)
    }

    /// The threads on `rung` whose rows speak for them: those with a tile in reading order,
    /// then the rest by worker.
    pub(super) fn threads_on(&self, rung: Rung) -> Vec<ThreadWait> {
        let tiles: HashMap<ThreadId, TileRef> = self
            .items()
            .filter_map(|(worker, i)| match i.kind {
                ItemKind::Thread { thread } => Some((thread, TileRef { worker, item: i.id })),
                _ => None,
            })
            .collect();
        let mut out: Vec<(Option<slopty_client::layout::Pos>, ThreadWait)> = self
            .thread_stands()
            .filter(|(_, stand)| stand.rung == rung)
            .map(|(thread, stand)| {
                let tile = tiles.get(&thread).copied();
                let pos = tile.and_then(|t| self.layout.position(t));
                (pos, ThreadWait { worker: stand.worker, thread, tile })
            })
            .collect();
        out.sort_by_key(|(pos, w)| {
            (pos.is_none(), pos.map(|p| (p.workspace, p.column, p.tile)), w.worker, w.thread)
        });
        out.into_iter().map(|(_, w)| w).collect()
    }

    /// One line of what `session`'s agent does, for the navigator and the overview: the last
    /// line its thread's agent wrote. The agent's words are Markdown, and the line says them as
    /// plain words.
    pub(super) fn face_summary(&self, session: SessionId) -> Option<String> {
        self.thread_line(self.session_thread(session)?).map(crate::markdown::plain_line)
    }
}

/// Where a thread stands as its worker's table row says it, for the navigator's glyph, the
/// header's pill, the bell and the status bar: a thread driven over a protocol has no
/// terminal whose agent status could say it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct ThreadStand {
    /// The worker whose agent runs it.
    pub worker: WorkerKey,
    /// Its rung on the attention ladder, its subagents' folded in.
    pub rung: Rung,
    /// Its agent has exited.
    pub exited: bool,
    /// What it asks while it waits on the person: its first open request.
    pub asks: Option<RequestCard>,
    /// Its terminal, whose agent status speaks for it where it has one.
    pub terminal: Option<SessionId>,
    /// When its row last changed, by its worker's clock.
    pub since: slopty_core::WallMs,
}

impl ThreadStand {
    /// The one vocabulary's mark for its rung; none at rest.
    pub(super) const fn status(&self) -> Option<Status> {
        match self.rung {
            Rung::NeedsYou => Some(Status::NeedsYou),
            Rung::Failed => Some(Status::Failed),
            Rung::Working => Some(Status::Working),
            Rung::Waiting => Some(Status::Running),
            Rung::ToReview => Some(Status::Done),
            Rung::Idle => None,
        }
    }

    /// Its state in a word or two, as a terminal agent's pill says it ("Needs approval",
    /// "Has a question"); none at rest.
    pub(super) fn word(&self) -> Option<&'static str> {
        if self.rung != Rung::NeedsYou {
            return self.rung.word();
        }
        Some(match self.asks.as_ref().map(|a| a.kind.as_str()) {
            Some(slopty_proto::thread::Request::APPROVAL) => "Needs approval",
            Some(slopty_proto::thread::Request::QUESTION) => "Has a question",
            Some(slopty_proto::thread::Request::ELICITATION) => "Needs input",
            _ => "Needs you",
        })
    }
}

/// A thread that waits on the person, for the bell, *Needs you* and the Dock's count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct ThreadWait {
    /// The worker whose agent runs it.
    pub worker: WorkerKey,
    /// The thread.
    pub thread: ThreadId,
    /// Its tile here, if one shows it.
    pub tile: Option<TileRef>,
}

/// What a thread tile's header says before its worker's table names the thread.
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
                exited: matches!(r.status.liveness, slopty_proto::thread::Liveness::Exited { .. }),
                asks: r.requests.first().cloned(),
                terminal: r.terminal,
                since: r.updated_ms,
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
