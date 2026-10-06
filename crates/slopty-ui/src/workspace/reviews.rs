//! Review tiles: a thread's changes, opened from its thread view as a tile of their own beside
//! the agent's (`ItemKind::Review`), and a folder's, with no thread (`ItemKind::Changes`).
//!
//! "Review changes" opens the changes of the focused folder, or of the repository the focused
//! shell is in, as a tile on that machine, or goes to the one already open there.
//!
//! A thread view asks for its review; the faces make the review's view
//! ([`WorkspaceView::take_review`]) and this side opens it as an item on the agent's worker, or
//! goes to the one already open, so a review tile is placed, restored and closed like any other. A
//! tile gone, closed here or by another client, lets the thread go
//! ([`WorkspaceView::review_closed`]). Comments sent from it go to the agent, so the keyboard goes
//! back to the agent's tile, where the answer shows; comments added to the message land at the
//! end of a draft of the thread, with the keyboard, to go with more words: its terminal's tile
//! turned to its thread, its own tile, or one opened for it. Either way the review lets them go
//! only once the worker or a composer took them. A file's Open, from its own menu, opens it in a
//! tile on the review's machine, and a line's author opens its thread, from a thread's review
//! and a folder's alike.
//!
//! A folder's comments go to a new agent there ([`WorkspaceView::review_to_new_agent`]): the
//! agent the folder last ran, its start's composer holding them quoted, to be added to before
//! ↵ sends them as its first message.

use std::collections::{HashMap, HashSet};

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use slopty_client::layout::WorkerKey;
use slopty_core::{ItemId, SessionId};
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::thread::wire::ThreadRow;
use slopty_proto::thread::{AgentId, ThreadId};

use super::WorkspaceView;
use super::actions::{ReviewChanges, StartThread};
use crate::conversation::thread::ThreadView;
use crate::review::{ReviewEvent, ReviewView};

/// The palette's line that opens a folder's changes.
pub(super) const REVIEW_CHANGES: &str = "Review changes";

/// What the workspace keeps of review tiles.
#[derive(Default)]
pub(super) struct Reviews {
    /// The threads with a review item, as the registries said last.
    shown: HashSet<ThreadId>,
    /// The threads whose review item this client proposed and the registry has not yet sent.
    opening: HashSet<ThreadId>,
    /// Each hosted review view's events, heard while its tile is there.
    hearing: HashMap<ThreadId, Subscription>,
    /// The view of each folder's changes tile, kept and heard while its tile is there.
    changes: HashMap<ItemId, (Entity<ReviewView>, Subscription)>,
}

impl WorkspaceView {
    /// Open the reviews thread views asked for since the last frame, in order: go to each
    /// one's tile when one is open, else add one on the worker whose agent runs the thread.
    pub(super) fn settle_reviews(&mut self, cx: &mut Context<Self>) {
        while let Some((key, thread, view)) = self.take_review() {
            self.hear_review(key, thread, &view, cx);
            if let Some(id) = self.review_item(thread) {
                self.go_to(id, cx);
                continue;
            }
            let item = Item {
                id: ItemId::new(),
                kind: ItemKind::Review { thread },
                name: None,
                facts: std::collections::BTreeMap::new(),
            };
            tracing::info!(id = %item.id, %thread, "open review");
            self.reviews.opening.insert(thread);
            self.propose(key, ItemOp::Add(item), cx);
            cx.notify();
        }
    }

    /// Open the reviews of `thread`'s runs on `key`, side by side, its own first: every thread
    /// the same first message started there in a worktree of the same clone ([`runs_of`]).
    pub(super) fn review_runs(&mut self, key: WorkerKey, thread: ThreadId, cx: &mut Context<Self>) {
        let hub = self.thread_hub(key, cx);
        let rows: Vec<ThreadRow> = hub.read(cx).threads().rows().rows.values().cloned().collect();
        let mut runs = runs_of(&rows, thread);
        runs.retain(|t| *t != thread);
        runs.insert(0, thread);
        self.faces_dirty = true;
        for run in runs {
            self.ask_review(key, run, None);
        }
        cx.notify();
    }

    /// Tell each thread view how many runs its first message has ([`ThreadView::set_runs`]),
    /// as its worker's table says.
    pub(super) fn count_runs(&self, cx: &mut Context<Self>) {
        let mut views: Vec<(WorkerKey, Entity<ThreadView>)> = Vec::new();
        for (session, view) in self.thread_faces() {
            if let Some(key) = self.worker_of_session(*session) {
                views.push((key, view.clone()));
            }
        }
        for (item, view) in self.thread_items() {
            if let Some(tile) = self.tile_of(*item) {
                views.push((tile.worker, view.clone()));
            }
        }
        let mut tables: HashMap<WorkerKey, Vec<ThreadRow>> = HashMap::new();
        for (key, view) in views {
            let Some(hub) = self.held_hub(key) else { continue };
            let rows = tables
                .entry(key)
                .or_insert_with(|| hub.read(cx).threads().rows().rows.values().cloned().collect());
            let runs = runs_of(rows, view.read(cx).thread()).len();
            view.update(cx, |v, cx| v.set_runs(runs, cx));
        }
    }

    /// Follow the registries: a review whose item is gone lets its thread go.
    pub(super) fn reconcile_reviews(&mut self) {
        let shown: HashSet<ThreadId> = self
            .items()
            .filter_map(|(_, item)| match item.kind {
                ItemKind::Review { thread } => Some(thread),
                _ => None,
            })
            .collect();
        self.reviews.opening.retain(|t| !shown.contains(t));
        let gone: Vec<ThreadId> = self
            .reviews
            .shown
            .iter()
            .filter(|t| !shown.contains(t) && !self.reviews.opening.contains(t))
            .copied()
            .collect();
        for thread in gone {
            self.reviews.hearing.remove(&thread);
            self.review_closed(thread);
        }
        self.reviews.shown = shown;
    }

    /// Give the keyboard to the review tile `item` shows, when its view is there.
    pub(super) fn focus_review(
        &self,
        thread: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.review_of(thread) {
            let handle = gpui::Focusable::focus_handle(view.read(cx), cx);
            window.focus(&handle, cx);
        }
    }

    /// The review item of `thread`, on whichever worker.
    fn review_item(&self, thread: ThreadId) -> Option<ItemId> {
        self.items().find_map(|(_, item)| match item.kind {
            ItemKind::Review { thread: t } if t == thread => Some(item.id),
            _ => None,
        })
    }

    /// The agent tile whose thread view shows `thread`.
    pub(super) fn thread_session(&self, thread: ThreadId, cx: &gpui::App) -> Option<SessionId> {
        self.workers
            .values()
            .flat_map(|w| w.sessions.keys())
            .copied()
            .find(|s| self.thread_face(*s).is_some_and(|v| v.read(cx).thread() == thread))
    }

    /// Hear the review of `thread` on `key`'s machine ([`Self::heard_review`]).
    fn hear_review(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        view: &Entity<ReviewView>,
        cx: &mut Context<Self>,
    ) {
        let hearing = cx.subscribe(view, move |this, view, event: &ReviewEvent, cx| {
            this.heard_review(key, &view, event, cx);
        });
        self.reviews.hearing.insert(thread, hearing);
    }

    /// What a review on `key`'s machine said. Comments sent take the keyboard to the thread's
    /// tile, and comments added to the message go to a composer of the thread, the review told
    /// whether one took them. A line's author opens its thread at its turn, and a file's Open
    /// opens it in a tile on the review's machine.
    fn heard_review(
        &mut self,
        key: WorkerKey,
        view: &Entity<ReviewView>,
        event: &ReviewEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ReviewEvent::CommentsSent { thread } => {
                if let Some(tile) = self.tile_of_thread(*thread) {
                    self.go_to(tile.item, cx);
                }
            }
            ReviewEvent::AddToMessage { thread, text, id } => {
                let (review, id) = (view.downgrade(), *id);
                self.quote_to_thread(*thread, text.clone(), cx, move |taken, cx| {
                    let _gone = review.update(cx, |v, cx| v.added(id, taken, cx));
                });
            }
            ReviewEvent::OpenThread(opens) => self.open_thread_at(*opens, cx),
            ReviewEvent::OpenFile { path } => self.open_file_on(Some(key), path, None, cx),
            // A folder's alone, heard with the window it starts in (`sync_changes`).
            ReviewEvent::NewAgent { .. } => {}
        }
    }

    /// A folder review's comments, `text`, for a new agent in `folder` on `key`'s machine: the
    /// agent the folder last ran, its start opened with them in its composer. Whether a
    /// composer took them, for the review: out of reach, or with no agent, they stay.
    pub(super) fn review_to_new_agent(
        &mut self,
        key: WorkerKey,
        folder: String,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(w) = self.workers.get(&key).filter(|w| w.link.is_none()) {
            let words = format!("{} is {}", w.name, w.status.text());
            self.show_notice(words, cx);
            return false;
        }
        let Some(agent) = self.folder_agent(key, &folder, cx).or_else(|| self.agent_for(key))
        else {
            self.show_notice(super::agent_start::NO_AGENT.to_owned(), cx);
            return false;
        };
        let start = StartThread { worker: key, agent, cwd: folder, worktree: false };
        let item = self.begin_start(start, window, cx);
        let Some(view) = self.starting.draft_view(item) else {
            return false;
        };
        view.update(cx, |v, cx| v.restore_draft(text, window, cx));
        true
    }

    /// The agent `folder` on `key` last ran, as its threads say: the newest that worked in the
    /// folder itself, else in its repository, among those the machine can start.
    fn folder_agent(&self, key: WorkerKey, folder: &str, cx: &gpui::App) -> Option<AgentId> {
        let startable = self.startable_on(key);
        let hub = self.held_hub(key)?.read(cx);
        let home = self.home_of(key);
        let folder = full_path(folder, home);
        let mut rows: Vec<&ThreadRow> = hub
            .threads()
            .rows()
            .rows
            .values()
            .filter(|r| r.parent.is_none() && startable.contains(&r.agent))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.updated_ms));
        let at = |path: Option<&String>| path.is_some_and(|p| full_path(p, home) == folder);
        let here = rows.iter().find(|r| at(r.cwd.as_ref()));
        here.or_else(|| rows.iter().find(|r| at(r.repo.as_ref()))).map(|r| r.agent.clone())
    }

    /// The folder whose changes "Review changes" opens from the focus: a folder tile's, or the
    /// repository the focused shell stands in; with the machine.
    pub(super) fn changes_here(&self) -> Option<(WorkerKey, String)> {
        use slopty_proto::items::ItemKind;
        let tile = self.focused()?;
        let path = match &self.item(tile)?.kind {
            ItemKind::Folder { path } | ItemKind::Changes { path, .. } => path.clone(),
            ItemKind::Terminal { session } => self.summary(*session)?.repo.clone()?,
            _ => return None,
        };
        Some((tile.worker, path))
    }

    /// "Review changes": the focused folder's changes, in their own tile on its machine; the
    /// one open there already for that folder takes the focus instead.
    pub(super) fn review_changes(
        &mut self,
        _: &ReviewChanges,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((key, path)) = self.changes_here() else { return };
        self.open_changes(key, path, None, cx);
    }

    /// The changes of `path` on `key`, in their own tile, against `against` when it names a
    /// branch (the whole branch since it left it); the one open there already for that folder
    /// and branch takes the focus instead.
    pub(super) fn open_changes(
        &mut self,
        key: WorkerKey,
        path: String,
        against: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let open = self.layout.tiles().find_map(|t| match &self.item(t)?.kind {
            ItemKind::Changes { path: p, against: a }
                if t.worker == key && *p == path && *a == against =>
            {
                Some(t.item)
            }
            _ => None,
        });
        if let Some(item) = open {
            self.go_to(item, cx);
            return;
        }
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Changes { path, against },
            name: None,
            facts: std::collections::BTreeMap::new(),
        };
        tracing::info!(id = %item.id, %key, "open changes");
        self.propose(key, ItemOp::Add(item), cx);
        cx.notify();
    }

    /// Make the view of each folder's changes tile, and let go of those whose tile is gone.
    pub(super) fn sync_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tiled: Vec<(ItemId, WorkerKey, String, Option<String>)> = self
            .layout
            .tiles()
            .filter_map(|t| match &self.item(t)?.kind {
                ItemKind::Changes { path, against } => {
                    Some((t.item, t.worker, path.clone(), against.clone()))
                }
                _ => None,
            })
            .collect();
        for (item, key, path, against) in &tiled {
            if self.reviews.changes.contains_key(item) {
                continue;
            }
            let hub = self.thread_hub(*key, cx);
            let theme = self.theme.clone();
            let path = path.clone();
            let view = cx.new(|cx| match against.clone() {
                Some(branch) => ReviewView::branch(hub, path, branch, theme, window, cx),
                None => ReviewView::folder(hub, path, theme, window, cx),
            });
            let key = *key;
            let hearing = cx.subscribe_in(&view, window, move |this, view, event, window, cx| {
                let ReviewEvent::NewAgent { folder, text, id } = event else {
                    this.heard_review(key, view, event, cx);
                    return;
                };
                let taken = this.review_to_new_agent(key, folder.clone(), text, window, cx);
                view.update(cx, |v, cx| v.added(*id, taken, cx));
            });
            self.reviews.changes.insert(*item, (view, hearing));
        }
        self.reviews.changes.retain(|item, _| tiled.iter().any(|(i, ..)| i == item));
    }

    /// The view of the folder's changes tile `item`, once made.
    #[must_use]
    pub fn changes_view(&self, item: ItemId) -> Option<&Entity<ReviewView>> {
        self.reviews.changes.get(&item).map(|(view, _)| view)
    }

    /// Every folder's changes tile's view.
    pub(super) fn changes_views(&self) -> impl Iterator<Item = &Entity<ReviewView>> {
        self.reviews.changes.values().map(|(view, _)| view)
    }
}

/// `path` whole, its leading `~` spelled as `home`, with no trailing `/`: two spellings of one
/// folder compare equal.
fn full_path(path: &str, home: Option<&str>) -> String {
    let path = path.trim_end_matches('/');
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.trim_end_matches('/'))
        }
        _ => path.to_owned(),
    }
}

/// Where a thread's worktree came from, as its runs share it: the folder its clone keeps
/// worktrees in and the name's words, without the four hex digits that tell runs apart
/// (`.claude/worktrees/<words>-<hex>`, [`super::starting::worktree_name`]). `None` for a thread
/// in no such worktree, or one whose name is only its agent's, which no message named.
fn run_key(row: &ThreadRow) -> Option<(&str, &str)> {
    const DIR: &str = "/.claude/worktrees/";
    let path = row.repo.as_deref().or(row.cwd.as_deref())?;
    let at = path.find(DIR)?;
    let (clone, rest) = (path.get(..at)?, path.get(at.saturating_add(DIR.len())..)?);
    let name = rest.split('/').next()?;
    let (words, hex) = name.rsplit_once('-')?;
    let tail = hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit());
    let agent: String =
        row.agent.0.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    (tail && !words.is_empty() && words != agent).then_some((clone, words))
}

/// The runs `thread`'s first message has among `rows`, itself among them: the threads in
/// worktrees of the same clone named by the same words ([`run_key`]), subagents aside. Only
/// `thread` when it has none.
pub(super) fn runs_of(rows: &[ThreadRow], thread: ThreadId) -> Vec<ThreadId> {
    let Some(mine) = rows.iter().find(|r| r.id == thread) else { return vec![thread] };
    let Some(key) = run_key(mine) else { return vec![thread] };
    let mut runs: Vec<ThreadId> = rows
        .iter()
        .filter(|r| r.parent.is_none() && run_key(r) == Some(key))
        .map(|r| r.id)
        .collect();
    if !runs.contains(&thread) {
        runs.push(thread);
    }
    runs
}
