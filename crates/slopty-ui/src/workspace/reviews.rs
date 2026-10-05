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
//! end of the agent's draft, with the keyboard, to go with more words.

use std::collections::{HashMap, HashSet};

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use slopty_client::layout::WorkerKey;
use slopty_core::{ItemId, SessionId};
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::thread::ThreadId;

use super::WorkspaceView;
use super::actions::ReviewChanges;
use crate::review::{ReviewEvent, ReviewView};

/// The palette's line that opens a folder's changes.
pub(super) const REVIEW_CHANGES: &str = "Review changes";

/// What the strip keeps of review tiles.
#[derive(Default)]
pub(super) struct Reviews {
    /// The threads with a review item, as the registries said last.
    shown: HashSet<ThreadId>,
    /// The threads whose review item this client proposed and the registry has not yet sent.
    opening: HashSet<ThreadId>,
    /// Each hosted review view's events, heard while its tile is there.
    hearing: HashMap<ThreadId, Subscription>,
    /// The view of each folder's changes tile, kept while its tile is there.
    changes: HashMap<ItemId, Entity<ReviewView>>,
}

impl WorkspaceView {
    /// Open the review a thread view asked for since the last frame: go to its tile when one is
    /// open, else add one on the worker whose agent runs the thread.
    pub(super) fn settle_reviews(&mut self, cx: &mut Context<Self>) {
        let Some((key, thread, view)) = self.take_review() else { return };
        self.hear_review(thread, &view, cx);
        if let Some(id) = self.review_item(thread) {
            self.go_to(id, cx);
            return;
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

    /// Hear the review of `thread`: comments sent take the keyboard to the agent's tile.
    fn hear_review(&mut self, thread: ThreadId, view: &Entity<ReviewView>, cx: &mut Context<Self>) {
        let hearing = cx.subscribe(view, |this, _view, event: &ReviewEvent, cx| match event {
            ReviewEvent::CommentsSent { thread } => {
                if let Some(session) = this.thread_session(*thread, cx) {
                    this.reveal_session(session, cx);
                }
            }
            ReviewEvent::AddToMessage { thread, text } => match this.thread_session(*thread, cx) {
                Some(session) => this.quote_to_agent(session, text.clone(), cx),
                None => {
                    this.show_notice("Open the agent's tile to add to its message".to_owned(), cx);
                }
            },
            ReviewEvent::OpenThread(opens) => this.open_thread_at(*opens, cx),
        });
        self.reviews.hearing.insert(thread, hearing);
    }

    /// The folder whose changes "Review changes" opens from the focus: a folder tile's, or the
    /// repository the focused shell stands in; with the machine.
    pub(super) fn changes_here(&self) -> Option<(WorkerKey, String)> {
        use slopty_proto::items::ItemKind;
        let tile = self.focused()?;
        let path = match &self.item(tile)?.kind {
            ItemKind::Folder { path } | ItemKind::Changes { path } => path.clone(),
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
        let open = self.layout.tiles().find_map(|t| match &self.item(t)?.kind {
            ItemKind::Changes { path: p } if t.worker == key && *p == path => Some(t.item),
            _ => None,
        });
        if let Some(item) = open {
            self.go_to(item, cx);
            return;
        }
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Changes { path },
            name: None,
            facts: std::collections::BTreeMap::new(),
        };
        tracing::info!(id = %item.id, %key, "open changes");
        self.propose(key, ItemOp::Add(item), cx);
        cx.notify();
    }

    /// Make the view of each folder's changes tile, and let go of those whose tile is gone.
    pub(super) fn sync_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tiled: Vec<(ItemId, WorkerKey, String)> = self
            .layout
            .tiles()
            .filter_map(|t| match &self.item(t)?.kind {
                ItemKind::Changes { path } => Some((t.item, t.worker, path.clone())),
                _ => None,
            })
            .collect();
        for (item, key, path) in &tiled {
            if self.reviews.changes.contains_key(item) {
                continue;
            }
            let hub = self.thread_hub(*key, cx);
            let theme = self.theme.clone();
            let path = path.clone();
            let view = cx.new(|cx| ReviewView::folder(hub, path, theme, window, cx));
            self.reviews.changes.insert(*item, view);
        }
        self.reviews.changes.retain(|item, _| tiled.iter().any(|(i, ..)| i == item));
    }

    /// The view of the folder's changes tile `item`, once made.
    #[must_use]
    pub fn changes_view(&self, item: ItemId) -> Option<&Entity<ReviewView>> {
        self.reviews.changes.get(&item)
    }

    /// Every folder's changes tile's view.
    pub(super) fn changes_views(&self) -> impl Iterator<Item = &Entity<ReviewView>> {
        self.reviews.changes.values()
    }
}
