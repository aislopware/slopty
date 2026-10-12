//! A project's board in a tile of its own, for when its orchestrator's tile cannot show it: the
//! project has no orchestrator (one named with "Name this project…"), its orchestrator's agent
//! has ended, or its machine is away. The board is the server's, so the person can still read,
//! tell, move and cancel the tasks while no orchestrator is there.
//!
//! Where the orchestrator's tile can show the board, it still does, beside its thread or terminal.
//! A board tile belongs to no machine. It sits under [`BOARD_WORKER`], a key no worker has, and
//! is saved with the layout (`Saved::boards`) so a relaunch puts it back.

use std::collections::HashMap;

use gpui::accesskit::Role;
use gpui::{
    Context, ElementId, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_client::layout::{Saved, SavedBoard, TileRef, WorkerKey};
use slopty_core::ItemId;
use slopty_kit::draw::Draw;
use slopty_kit::icons::Symbol;
use slopty_proto::project::ProjectId;

use super::WorkspaceView;
use super::area::Placed;
use super::tile::BodyState;

/// The key board tiles sit under: the nil id, which no worker has.
pub(super) const BOARD_WORKER: WorkerKey = WorkerKey::new(0);

/// What a board tile says while the server is not linked, or no longer has its project.
pub(crate) fn board_away(project: &ProjectId, linked: bool) -> String {
    if linked {
        format!("{project} is no longer on the server")
    } else {
        format!("{project}'s board shows once the server is back")
    }
}

/// The board tiles the last run left, by the project each shows.
pub(super) fn saved(saved: Option<&Saved>) -> HashMap<ItemId, ProjectId> {
    saved
        .iter()
        .flat_map(|s| &s.boards)
        .filter(|b| b.tile.worker == BOARD_WORKER)
        .map(|b| (b.tile.item, b.project.clone()))
        .collect()
}

impl WorkspaceView {
    /// The project whose board `item`'s tile shows, when it is a board tile.
    #[must_use]
    pub(super) fn board_tile(&self, item: ItemId) -> Option<&ProjectId> {
        self.projects.tiles.get(&item)
    }

    /// Show `project`'s board in a tile of its own, and go there: the one open already, else
    /// a new one in a tab of its own.
    pub(super) fn open_board_tile(&mut self, project: &ProjectId, cx: &mut Context<Self>) {
        let open = self
            .projects
            .tiles
            .iter()
            .find(|(_, p)| *p == project)
            .map(|(item, _)| TileRef { worker: BOARD_WORKER, item: *item })
            .filter(|tile| self.layout.contains(*tile));
        if let Some(tile) = open {
            self.focus_tile(tile, cx);
        } else {
            let item = ItemId::new();
            self.projects.tiles.insert(item, project.clone());
            self.open_as(TileRef { worker: BOARD_WORKER, item }, super::tabs::Opening::Tab);
            self.after_focus_moved(cx);
            self.layout_touched(cx);
        }
        self.projects.focus.insert(project.clone());
        self.projects.dirty = true;
        self.changed(cx);
        cx.notify();
    }

    /// The board tiles in the layout, as `layout.json` keeps them.
    pub(super) fn saved_boards(&self) -> Vec<SavedBoard> {
        let mut saved: Vec<SavedBoard> = self
            .projects
            .tiles
            .iter()
            .map(|(item, project)| SavedBoard {
                tile: TileRef { worker: BOARD_WORKER, item: *item },
                project: project.clone(),
            })
            .filter(|b| self.layout.contains(b.tile))
            .collect();
        saved.sort_by_key(|b| b.tile.item);
        saved
    }

    /// A board tile as the frame places it: its project's board, or what keeps it from showing.
    pub(super) fn render_board_tile(
        &self,
        placed: &Placed,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let tile = placed.tile;
        let project = self.board_tile(tile.item)?;
        let id = tile.item;
        let board = self.projects.mirror.get(project);
        let title = SharedString::from(
            board.map_or_else(|| project.to_string(), |b| b.project.title.clone()),
        );
        // Its pane's tab bar of one cell, as every pane's (`tile::render_header`).
        let header = (!self.phone)
            .then(|| self.one_tab_bar(placed, Symbol::Checklist, title.clone(), None, cx));
        let body = div().flex_1().min_h_0().w_full();
        let body = if let Some(view) = board.and_then(|_| self.projects.views.get(project)) {
            self.hand_over(cx, view, super::area::Handed::Board { beside: false }, |v, cx| {
                v.set_beside(false, cx);
            });
            body.child(self.body_view(view, placed, cx))
        } else {
            let linked = self.projects.caller.is_some();
            let state = BodyState::Away(board_away(project, linked).into());
            body.relative().child(self.render_state_pill(tile, &state, true, cx))
        };
        Some(
            div()
                .id(ElementId::Uuid(*id.as_uuid()))
                .debug_selector(move || format!("item-{}", id.as_uuid()))
                .role(Role::Group)
                .aria_label(title)
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| this.click_tile(tile, cx)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .children(header)
                        .child(body)
                        .relative()
                        .size_full()
                        .overflow_hidden(),
                )
                .into_any_element(),
        )
    }

    /// ⌘W on a board tile: it goes, and its project's board view with it once nothing else
    /// shows it. False when `tile` is no board tile.
    pub(super) fn close_board_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) -> bool {
        if self.projects.tiles.remove(&tile.item).is_none() {
            return false;
        }
        self.layout.remove(tile);
        self.projects.dirty = true;
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
        true
    }
}
