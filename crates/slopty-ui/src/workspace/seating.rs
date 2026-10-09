//! Where an orchestrator's helpers sit (`MonoCode` audit row 21, `queueWorkerPanes`).
//!
//! A task's agent that an orchestrator started comes from elsewhere: the worker's list, or
//! the server's start. Any other tile from elsewhere is a background tab of its project
//! ([`slopty_client::layout::Tiling::arrive`]). A task's agent instead takes a pane beside its
//! orchestrator's tile, in that tile's tab, and the next ones share that pane as its tabs, so
//! the lead and its helpers read as one piece of work
//! ([`slopty_client::layout::Tiling::arrive_beside`]). The focus stays where the person had it.
//!
//! The server may name the session a task's agent after its tile came: such a tile, still alone
//! in the background tab it arrived in, is seated once the project says whose it is. A tile the
//! person has shown, moved or given company is theirs and stays. A phone, with one pane on
//! show, keeps every arrival a tab.

use slopty_client::layout::TileRef;
use slopty_core::SessionId;
use slopty_proto::items::ItemKind;

use super::WorkspaceView;

impl WorkspaceView {
    /// The tile `tile` helps: its project's orchestrator's, while `tile`'s agent works on one
    /// of that project's tasks.
    pub(super) fn lead_of(&self, tile: TileRef) -> Option<TileRef> {
        let session = self.tile_agent_session(tile)?;
        let (board, _) = self.projects.mirror.of_agent(session)?;
        let lead = board.project.orchestrator?;
        if lead.session == session {
            return None;
        }
        self.tile_of_session(lead.session).filter(|lead| *lead != tile)
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

    /// Seat `tile`, come from elsewhere, beside its lead; `false` when it has none placed or
    /// the layout keeps arrivals as tabs, for the caller to let it arrive as any tile does.
    pub(super) fn arrive_beside_lead(&mut self, tile: TileRef) -> bool {
        let Some(lead) = self.lead_of(tile) else { return false };
        let helpers: Vec<TileRef> =
            self.layout.tiles().filter(|t| self.lead_of(*t) == Some(lead)).collect();
        self.layout.arrive_beside(tile, lead, |t| helpers.contains(&t))
    }

    /// Seat the tiles that arrived before the project named them a task's agent, each still
    /// alone in its background tab; forget the ones the person has made theirs. `true` when
    /// one moved.
    pub(super) fn seat_helpers(&mut self, cx: &gpui::Context<Self>) -> bool {
        if self.projects.arrived.is_empty() || self.layout.is_phone() {
            return false;
        }
        let arrived: Vec<TileRef> = self.projects.arrived.iter().copied().collect();
        let mut moved = false;
        for tile in arrived {
            if !self.alone_in_background(tile) {
                self.projects.arrived.remove(&tile);
                continue;
            }
            if self.lead_of(tile).is_none() {
                continue;
            }
            self.projects.arrived.remove(&tile);
            let home = self.home_for(tile);
            self.layout.remove(tile);
            if self.arrive_beside_lead(tile) {
                moved = true;
            } else {
                self.layout.arrive(tile, &home);
            }
        }
        if moved {
            self.layout_touched(cx);
        }
        moved
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
