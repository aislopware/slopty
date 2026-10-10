//! Task agents are rows, not tiles (`.research/orchestrator-first-2026-10-10.md`, item 1).
//!
//! A task's agent that an orchestrator started comes from elsewhere: the worker's list, or the
//! server's start. Any other tile from elsewhere is a background tab of its project
//! ([`slopty_client::layout::Tiling::arrive`]). A task's agent takes no place in the tiling at
//! all: the person directs the orchestrator, and ten helpers each taking a pane or a tab is the
//! babysitting the direction leaves behind. It stays a row (the board's, a Needs-you or To
//! review row, a note), and focusing it opens it on demand as the helper preview: a tab that
//! the next helper opened takes over while it is still on show, so reading through the tasks
//! leaves one tab rather than a row of them ([`WorkspaceView::open_helper`]).
//!
//! The server may name the session a task's agent after its tile came: such a tile, still alone
//! in the background tab it arrived in, leaves the tiling once the project says whose it is. A
//! tile the person has shown, moved or given company is theirs and stays.

use slopty_client::layout::TileRef;
use slopty_core::SessionId;
use slopty_proto::items::ItemKind;

use super::WorkspaceView;
use super::tabs::Opening;

impl WorkspaceView {
    /// The tile `tile` helps: its project's orchestrator's, while `tile`'s agent works on one
    /// of that project's tasks. The navigator sets a helper the person opened in under it.
    pub(super) fn lead_of(&self, tile: TileRef) -> Option<TileRef> {
        let session = self.tile_agent_session(tile)?;
        let (board, _) = self.projects.mirror.of_agent(session)?;
        let lead = board.project.orchestrator?;
        if lead.session == session {
            return None;
        }
        self.tile_of_session(lead.session).filter(|lead| *lead != tile)
    }

    /// Whether `tile`'s agent works on one of a project's tasks.
    pub(super) fn is_task_agent(&self, tile: TileRef) -> bool {
        self.tile_agent_session(tile).is_some_and(|s| self.projects.mirror.of_agent(s).is_some())
    }

    /// The terminal session behind `tile`: its own, or its thread's.
    fn tile_agent_session(&self, tile: TileRef) -> Option<SessionId> {
        match self.item(tile)?.kind {
            ItemKind::Terminal { session } => Some(session),
            ItemKind::Thread { thread } | ItemKind::Review { thread } => {
                self.thread_terminal(thread)
            }
            _ => None,
        }
    }

    /// Take out of the tiling the tiles that arrived before the project named them a task's
    /// agent, each still alone in its background tab; forget the ones the person has made
    /// theirs. `true` when one left.
    pub(super) fn unseat_helpers(&mut self, cx: &gpui::Context<Self>) -> bool {
        if self.projects.arrived.is_empty() {
            return false;
        }
        let arrived: Vec<TileRef> = self.projects.arrived.iter().copied().collect();
        let mut moved = false;
        for tile in arrived {
            if !self.alone_in_background(tile) {
                self.projects.arrived.remove(&tile);
                continue;
            }
            if !self.is_task_agent(tile) {
                continue;
            }
            self.projects.arrived.remove(&tile);
            self.layout.remove(tile);
            moved = true;
        }
        if moved {
            self.layout_touched(cx);
        }
        moved
    }

    /// Open `tile`, an item the tiling does not hold (a task's agent), as the helper preview:
    /// in the place of the one before while that is still in the tab on show, else as a tab of
    /// its own in its project. The one it replaces leaves the tiling again, a row as before.
    pub(super) fn open_helper(&mut self, tile: TileRef) {
        let replaced = self.projects.helper.filter(|old| *old != tile).and_then(|old| {
            let shown = self.layout.shown_tab().is_some_and(|tab| tab.pane_of(old).is_some());
            shown.then(|| self.layout.position(old).map(|at| (old, at))).flatten()
        });
        match replaced {
            Some((old, at)) => {
                self.layout.remove(old);
                if !self.layout.put_back(tile, at) {
                    self.open_as(tile, Opening::Tab);
                }
            }
            None => self.open_as(tile, Opening::Tab),
        }
        self.projects.helper = Some(tile);
    }

    /// Whether `tile` is placed, out of sight, alone in its tab: where an arrival lands.
    fn alone_in_background(&self, tile: TileRef) -> bool {
        let Some(at) = self.layout.position(tile) else { return false };
        if self.layout.on_show(tile) {
            return false;
        }
        self.layout
            .projects()
            .get(at.project)
            .and_then(|p| p.tabs().iter().find(|t| t.id() == at.tab))
            .is_some_and(|tab| tab.tiles().count() == 1)
    }
}
