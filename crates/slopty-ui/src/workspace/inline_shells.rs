//! The shells behind threads' inline terminals ("!" first in an empty composer,
//! [`crate::conversation::thread::ThreadView::inline_terminal`]).
//!
//! A thread view asks for one in its folder; the shell is asked of the thread's machine as any
//! shell is, and its item, when it comes, is kept off the tiling: no tab, no pane, no row. Once
//! its terminal is attached, its view goes to the thread view that asked, which draws it in the
//! next turn's place. When the view is done with it (sent with a message, let go) or its shell
//! ends, the item goes and the shell with it.

use std::collections::{HashMap, VecDeque};

use gpui::{Context, Entity, WeakEntity};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::SessionId;
use slopty_proto::items::{ItemKind, ItemOp};

use super::WorkspaceView;
use super::tabs::Opening;
use crate::conversation::thread::ThreadView;

/// The inline terminals asked for, coming and on show.
#[derive(Default)]
pub(super) struct InlineShells {
    /// Asked of a machine, its shell not come yet: the view that asked, first asked first.
    asked: VecDeque<(WorkerKey, WeakEntity<ThreadView>)>,
    /// Come as an item, its terminal not attached yet.
    coming: Vec<(TileRef, WeakEntity<ThreadView>)>,
    /// On show in a thread view, by its shell.
    shown: HashMap<SessionId, (TileRef, WeakEntity<ThreadView>)>,
}

impl WorkspaceView {
    /// `view`, a thread on `key`, asked for a shell in `cwd` for its inline terminal.
    pub(super) fn open_inline(
        &mut self,
        key: WorkerKey,
        view: &Entity<ThreadView>,
        cwd: String,
        cx: &mut Context<Self>,
    ) {
        if !self.reachable_for(key, "the terminal", cx) {
            view.update(cx, ThreadView::inline_terminal_gone);
            return;
        }
        self.inline.asked.push_back((key, view.downgrade()));
        self.open_session_as(key, Some(cwd), Opening::Inline, cx);
    }

    /// The shell asked for an inline terminal came as `tile`: it waits off the tiling for its
    /// terminal, which then goes to the view that asked ([`Self::hand_inline`]).
    pub(super) fn inline_arrived(&mut self, tile: TileRef) {
        let Some(at) = self.inline.asked.iter().position(|(key, _)| *key == tile.worker) else {
            return;
        };
        if let Some((_, view)) = self.inline.asked.remove(at) {
            self.inline.coming.push((tile, view));
        }
    }

    /// Whether `tile` is an inline terminal's, which stands on no tab.
    pub(super) fn is_inline(&self, tile: TileRef) -> bool {
        self.inline.coming.iter().any(|(t, _)| *t == tile)
            || self.inline.shown.values().any(|(t, _)| *t == tile)
    }

    /// Give each inline terminal attached since to the view that asked for it. One whose view
    /// went meanwhile goes too.
    pub(super) fn hand_inline(&mut self, cx: &mut Context<Self>) {
        let coming = std::mem::take(&mut self.inline.coming);
        for (tile, view) in coming {
            let Some(ItemKind::Terminal { session }) = self.item(tile).map(|item| &item.kind)
            else {
                self.inline.coming.push((tile, view));
                continue;
            };
            let session = *session;
            let Some(terminal) = self.terminals.get(&session).cloned() else {
                self.inline.coming.push((tile, view));
                continue;
            };
            let Some(thread) = view.upgrade() else {
                self.let_go_inline(tile, session, cx);
                continue;
            };
            self.inline.shown.insert(session, (tile, view));
            thread.update(cx, |v, cx| v.inline_terminal(session, terminal, cx));
        }
    }

    /// The inline terminal on `session` is done with: its item goes, and its shell with it.
    pub(super) fn inline_done(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if let Some((tile, _)) = self.inline.shown.remove(&session) {
            self.let_go_inline(tile, session, cx);
        }
    }

    /// An inline terminal's shell ends and its item goes: nothing else shows it, so nothing
    /// is kept for ⌘Z.
    fn let_go_inline(&mut self, tile: TileRef, session: SessionId, cx: &mut Context<Self>) {
        self.close_session_on(tile.worker, session);
        self.propose(tile.worker, ItemOp::Remove(tile.item), cx);
    }

    /// `session` ended: an inline terminal's place in its thread goes, and its item.
    pub(super) fn inline_ended(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some((tile, view)) = self.inline.shown.remove(&session) else { return };
        if let Some(view) = view.upgrade() {
            view.update(cx, ThreadView::inline_terminal_gone);
        }
        self.propose(tile.worker, ItemOp::Remove(tile.item), cx);
    }
}
