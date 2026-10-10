//! The tabs' commands: new work in a tab of its own or in a pane split off the focused one
//! (⌘T, ⌘⇧T, ⌘D, ⌘⇧D), the tab's terminal (⌘⌥T), the steps that list the project's tabs and the
//! other projects, and closing the other tabs.
//!
//! A shell is the worker's to make, so where it goes is decided when its item comes, not when
//! it is asked for: each ask is queued with where it goes ([`Opening`]), and the shells this
//! client asked a worker for come back in the order they were asked. An ask the worker
//! refused leaves the queue, and a link that drops takes its asks with it.

use std::collections::VecDeque;
use std::rc::Rc;

use gpui::{App, Context, SharedString, WeakEntity, Window};
use slopty_client::layout::tiling::TabId;
use slopty_client::layout::{Drop, GroupKey, Pos, Side, Tab, TileRef, WorkerKey};
use slopty_core::ItemId;
use slopty_proto::RequestId;
use slopty_proto::items::ItemKind;

use super::actions::{
    CloseOtherTabs, MOVE_TO_PROJECT, MoveToProject, MoveToProjectOf, OTHER_TABS, OtherTabs,
    ShowTab, SplitDown, SplitRight, StartAgent, StartThread, TabTerminal,
};
use super::title_tabs::TitleTabsHost as _;
use super::{MenuEntry, MenuGroup, WorkspaceView};
use crate::palette::PaletteItem;

/// Where a shell this client asked for goes once its item comes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Opening {
    /// By the room rule beside the focused tile: what a tile opens.
    Beside,
    /// In a tab of its own (⌘⇧T).
    Tab,
    /// In a pane of its own on this side of the focused one (⌘D, ⌘⇧D).
    Split(Side),
    /// The terminal of the tab on show, below the whole tab (⌘⌥T's first press).
    Terminal,
    /// Where a drop on a pane of the tab on show said: a thread's row carried there.
    At(Drop),
    /// In the pane of the preview it replaces, beside it (`preview`).
    Into(Pos),
}

/// The shells asked of one worker and not come yet, the first asked first.
pub(super) type Openings = VecDeque<(RequestId, Opening)>;

impl WorkspaceView {
    /// ⌘T: an agent's composer in a tab of its own, on the focused tile's machine (or the one
    /// "+" chose) and in its folder, else where that machine last worked. The agent is the
    /// one last started there, else the first it offers. It starts in a new worktree when the
    /// last start on that machine, in the same repository, chose one ([`Self::worktree_again`]).
    /// The draft's place chip shows it and takes it back.
    pub(super) fn start_agent(
        &mut self,
        _: &StartAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((worker, cwd)) = self.new_tile_target() else { return };
        if !self.reachable_for(worker, "the agent", cx) {
            return;
        }
        let Some(agent) = self.agent_for(worker) else {
            self.show_notice(super::agent_start::NO_AGENT.to_owned(), cx);
            return;
        };
        let cwd = cwd.unwrap_or_else(|| {
            let latest =
                self.recent_places(Some(&agent), cx).into_iter().find(|p| p.worker == worker);
            latest.map_or_else(|| "~".to_owned(), |p| p.cwd)
        });
        let worktree = self.worktree_again(worker, &cwd, cx);
        self.begin_start(StartThread { worker, agent, cwd, worktree }, window, cx);
    }

    /// Whether a start at `cwd` on `worker` goes in a new worktree as the person's last one
    /// there did: that start made one, from the same repository `cwd` is in.
    fn worktree_again(&self, worker: WorkerKey, cwd: &str, cx: &App) -> bool {
        let Some(last) = self.starts.last().filter(|l| l.worktree && l.worker == worker) else {
            return false;
        };
        let repo = self.repo_at(worker, cwd, cx);
        repo.is_some() && repo == self.repo_at(worker, &last.cwd, cx)
    }

    /// ⌘D: a shell in a pane of its own right of the focused one, in its folder.
    pub(super) fn split_right(&mut self, _: &SplitRight, _w: &mut Window, cx: &mut Context<Self>) {
        self.new_terminal_as(Opening::Split(Side::Right), cx);
    }

    /// ⌘⇧D: a shell in a pane of its own below the focused one, in its folder.
    pub(super) fn split_down(&mut self, _: &SplitDown, _w: &mut Window, cx: &mut Context<Self>) {
        self.new_terminal_as(Opening::Split(Side::Bottom), cx);
    }

    /// ⌘⌥T: the tab's terminal put away or brought back, its shell going on; the first time, a
    /// shell asked for it on the focused tile's worker, in its folder. Pressed again before that
    /// shell comes, nothing more is asked.
    pub(super) fn tab_terminal(
        &mut self,
        _: &TabTerminal,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut shown = None;
        self.layout_action(cx, |l| shown = l.toggle_terminal());
        let asked =
            self.workers.values().any(|w| w.openings.iter().any(|(_, o)| *o == Opening::Terminal));
        if shown.is_none() && !asked && self.layout.shown_tab().is_some() {
            self.new_terminal_as(Opening::Terminal, cx);
        }
    }

    /// What a phone's title menu would list: the panes of the tab on show, and the project's
    /// tabs.
    pub(super) fn switch_counts(&self) -> (usize, usize) {
        let panes =
            self.layout.shown_tab().map_or(0, |t| t.panes().filter(|p| !p.hidden()).count());
        let tabs = self.layout.shown_project().map_or(0, |p| p.tabs().len());
        (panes, tabs)
    }

    /// A phone's title menu ([`super::titlebar::MenuKind::Switch`]): the panes of the tab on
    /// show by their shown tile, the focused one ticked, while there are two or more; then the
    /// project's tabs, the one on show ticked, while there are two or more.
    pub(super) fn switch_entries(&self, entity: &WeakEntity<Self>) -> Vec<MenuEntry> {
        let (panes, tabs) = self.switch_counts();
        let tick = |on: bool| SharedString::from(if on { "\u{2713}" } else { "" });
        let mut entries = Vec::new();
        if panes > 1 {
            let focused = self.focused();
            let shown = self.layout.shown_tab().into_iter().flat_map(Tab::panes);
            for tile in shown.filter(|p| !p.hidden()).filter_map(slopty_client::layout::Pane::shown)
            {
                let label = self.item(tile).map(|item| self.tile_title(item)).unwrap_or_default();
                let entity = entity.clone();
                entries.push(MenuEntry {
                    group: MenuGroup::Panes,
                    label: label.into(),
                    detail: tick(focused == Some(tile)),
                    run: Rc::new(move |_window: &mut Window, cx: &mut App| {
                        let _gone = entity.update(cx, |this, cx| this.focus_tile(tile, cx));
                    }),
                });
            }
        }
        if tabs > 1 {
            for tab in self.title_tabs() {
                let (id, entity) = (tab.id, entity.clone());
                entries.push(MenuEntry {
                    group: MenuGroup::Tabs,
                    label: tab.title,
                    detail: tick(tab.shown),
                    run: Rc::new(move |_window: &mut Window, cx: &mut App| {
                        let _gone = entity.update(cx, |this, cx| this.show_tab(id, cx));
                    }),
                });
            }
        }
        entries
    }

    /// A shell on the focused tile's worker (or the one "+" chose), in the focused shell's
    /// directory when it is on that worker, going where `opening` says.
    pub(super) fn new_terminal_as(&mut self, opening: Opening, cx: &mut Context<Self>) {
        let Some((key, cwd)) = self.new_tile_target() else { return };
        self.open_session_as(key, cwd, opening, cx);
    }

    /// Where `id`, which this client caused on `key`, goes: a shell where its ask said, the
    /// first one not come yet; anything else beside the focus.
    pub(super) fn opening_of(&mut self, key: WorkerKey, id: ItemId) -> Opening {
        if let Some(at) = self.preview_place(id) {
            return Opening::Into(at);
        }
        let Some(w) = self.workers.get_mut(&key) else { return Opening::Beside };
        if let Some(drop) = w.dropped.remove(&id) {
            return Opening::At(drop);
        }
        let shell =
            w.doc.get(id).is_some_and(|item| matches!(item.kind, ItemKind::Terminal { .. }));
        let queued = if shell { w.openings.pop_front() } else { None };
        queued.map_or(Opening::Beside, |(_, opening)| opening)
    }

    /// The ask under `request` on `key` failed: its shell never comes.
    pub(super) fn opening_failed(&mut self, key: WorkerKey, request: RequestId) {
        if let Some(w) = self.workers.get_mut(&key) {
            w.openings.retain(|(r, _)| *r != request);
        }
    }

    /// Put `tile`, opened here, where `opening` says, in the project on show; with nothing on
    /// show, in a tab of its own project.
    pub(super) fn open_as(&mut self, tile: TileRef, opening: Opening) {
        let home = self.home_here(tile);
        match opening {
            Opening::Beside => self.layout.open_beside(tile, &home),
            Opening::Tab => {
                self.layout.new_tab(tile, &home);
            }
            Opening::Split(side) => self.layout.split_focused(tile, side, &home),
            Opening::Terminal => {
                if !self.layout.set_terminal(tile) {
                    self.layout.new_tab(tile, &home);
                }
            }
            // The pane dropped on may have gone since: then beside the focus, as anything else.
            Opening::At(drop) => {
                if self.layout.place(tile, drop) {
                    self.layout.focus(tile);
                } else {
                    self.layout.open_beside(tile, &home);
                }
            }
            Opening::Into(at) => {
                if !self.layout.put_back(tile, at) {
                    self.layout.open_beside(tile, &home);
                }
            }
        }
    }

    /// The project what is opened here goes to: the one on show, else `tile`'s own.
    pub(super) fn home_here(&self, tile: TileRef) -> GroupKey {
        match self.layout.shown_project() {
            Some(project) => project.home().clone(),
            None => self.home_for(tile),
        }
    }

    /// "Close other tabs": every tab of the project on show but the one on show, and what is in
    /// them, then back to the one kept. A shell whose command runs asks first, in its tab.
    pub(super) fn close_other_tabs(
        &mut self,
        _: &CloseOtherTabs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.layout.shown_project() else { return };
        let Some(kept) = project.shown().map(Tab::id) else { return };
        let others: Vec<_> = project.tabs().iter().map(Tab::id).filter(|id| *id != kept).collect();
        let back = self.focused();
        for id in others {
            self.close_title_tab(id, window, cx);
        }
        match back {
            Some(tile) if self.layout.contains(tile) => self.focus_tile(tile, cx),
            _ => self.layout_action(cx, |l| l.show_tab(kept)),
        }
    }

    /// "Other tabs…": a line for each tab of the project on show, by its focused work, the one
    /// on show ticked.
    pub(super) fn other_tabs(
        &mut self,
        _: &OtherTabs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines = self
            .title_tabs()
            .into_iter()
            .map(|tab| {
                let mut line = PaletteItem::new(&tab.title, Box::new(ShowTab { id: tab.id }), &[]);
                if tab.shown {
                    "\u{2713}".clone_into(&mut line.keys);
                }
                line
            })
            .collect();
        self.open_step(lines, OTHER_TABS, window, cx);
    }

    /// A tab picked in "Other tabs…".
    pub(super) fn show_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.layout_action(cx, |l| l.show_tab(id));
    }

    /// "Move to project…": a line for each project but the focused tile's, by name.
    pub(super) fn move_to_project(
        &mut self,
        _: &MoveToProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.focused() else { return };
        let here = self.layout.position(tile).map(|p| p.project);
        let lines = (0..self.layout.projects().len())
            .filter(|ix| Some(*ix) != here)
            .filter_map(|ix| {
                let home = self.layout.projects().get(ix)?.home().clone();
                let name = self.project_name_at(ix);
                Some(PaletteItem::new(&name, Box::new(MoveToProjectOf { home }), &[]))
            })
            .collect();
        self.open_step(lines, MOVE_TO_PROJECT, window, cx);
    }

    /// A project picked in "Move to project…": the focused tile goes to a tab of its own
    /// there, and the focus with it.
    pub(super) fn move_to_project_of(
        &mut self,
        to: &MoveToProjectOf,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.focused() else { return };
        let home = to.home.clone();
        self.layout_action(cx, |l| l.move_to_project(tile, &home));
    }
}
