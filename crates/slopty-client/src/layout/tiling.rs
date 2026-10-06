//! Projects, their tabs, and where new work opens: the whole workspace as a pure model.
//!
//! A project ([`crate::groups`]' [`GroupKey`]: a declared project, else a repository, else a
//! folder, else a machine) owns its tabs, and each tab holds a tiling layout ([`Tab`]). One
//! project is on show; its tabs are the title bar's, and it comes back on the tab it was left
//! on (`MonoCode`'s `projectReturn.ts`). Tabs visited are kept in order for back and forward.
//!
//! Where new work opens:
//! - a start, a terminal: a new tab ([`Tiling::new_tab`]);
//! - what opens from a tile follows the room rule beside it ([`Tiling::open_beside`]): a tab of the
//!   pane to its right; else a sibling to its right while every pane of the row keeps
//!   [`Room::min_w`]; else a split below while both halves keep [`Room::min_h`]; else a tab of its
//!   own pane;
//! - a tile that arrives from elsewhere is a background tab in its project, and the focus stays
//!   where it was ([`Tiling::arrive`]).
//!
//! A placed tile never moves when its facts change; only the person moves it. Pure, as the tree
//! is: the caller gives the area ([`Tiling::set_area`]) and reads a [`TabFrame`] back. Below
//! [`TilingConfig::phone_below`] the tab draws only its focused pane, and nothing splits.

use serde::{Deserialize, Serialize};
use slopty_core::ItemId;

use super::tree::{Node, Pane, PaneId, Room, Side, Split, SplitAxis, Tab, TabFrame, Taken};
use super::{GroupKey, Rect, TileRef, WorkerKey};

/// How far back the tabs visited are kept.
const VISITS_KEPT: usize = 100;

/// The share of a drop's pane, from its nearer edge, that splits on that edge (Zed's
/// `drop_target_size`): the middle joins its tabs.
pub const DROP_BAND: f32 = 0.2;

/// The tab's terminal's share of the height (⌘⌥T).
const TERMINAL_SHARE: f32 = 1.0 / 3.0;

/// A tab's identity, stable across edits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TabId(u64);

impl TabId {
    /// Its number: unique among one [`Tiling`]'s tabs.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The model's constants.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TilingConfig {
    /// The least room a pane takes.
    pub room: Room,
    /// An area narrower than this is a phone's: one pane shows, and nothing splits.
    pub phone_below: f32,
}

impl Default for TilingConfig {
    fn default() -> Self {
        Self { room: Room::POINTER, phone_below: 700.0 }
    }
}

/// A project: its tabs, and the one it was left on.
#[derive(Clone, PartialEq, Debug)]
pub struct Project {
    home: GroupKey,
    name: Option<String>,
    tabs: Vec<Tab>,
    shown: usize,
}

impl Project {
    const fn new(home: GroupKey) -> Self {
        Self { home, name: None, tabs: Vec::new(), shown: 0 }
    }

    /// Its group.
    #[must_use]
    pub const fn home(&self) -> &GroupKey {
        &self.home
    }

    /// The name the person gave it, if any.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Its tabs, as the title bar runs them.
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// The tab it shows: the one it was left on.
    #[must_use]
    pub fn shown(&self) -> Option<&Tab> {
        self.tabs.get(self.shown)
    }

    /// Which of its tabs it shows.
    #[must_use]
    pub const fn shown_index(&self) -> usize {
        self.shown
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id() == id)
    }
}

/// Where a tile is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pos {
    /// The project, by index.
    pub project: usize,
    /// The tab.
    pub tab: TabId,
    /// The pane.
    pub pane: PaneId,
}

/// Where a dragged tile lands on a pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Drop {
    /// The pane.
    pub pane: PaneId,
    /// The edge it splits on; `None` joins the pane's tabs.
    pub edge: Option<Side>,
}

/// The whole workspace.
#[derive(Clone, PartialEq, Debug)]
pub struct Tiling {
    config: TilingConfig,
    projects: Vec<Project>,
    /// The project on show.
    shown: Option<usize>,
    /// The tabs visited, oldest first, and where back and forward stand among them.
    visits: Vec<TabId>,
    at: usize,
    /// The area a tab is laid out in.
    area: Rect,
    /// The next id to give a pane or a tab.
    next: u64,
}

impl Tiling {
    /// An empty workspace.
    #[must_use]
    pub const fn new(config: TilingConfig) -> Self {
        Self {
            config,
            projects: Vec::new(),
            shown: None,
            visits: Vec::new(),
            at: 0,
            area: Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            next: 1,
        }
    }

    /// Its constants.
    #[must_use]
    pub const fn config(&self) -> TilingConfig {
        self.config
    }

    /// Change the least room a pane takes (touch, or a pointer).
    pub const fn set_room(&mut self, room: Room) {
        self.config.room = room;
    }

    /// The area a tab is laid out in, `w` by `h` points from the origin.
    pub const fn set_area(&mut self, w: f32, h: f32) {
        self.area = Rect { x: 0.0, y: 0.0, w, h };
    }

    /// The area a tab is laid out in.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    /// Whether the area is a phone's: one pane at a time.
    #[must_use]
    pub fn is_phone(&self) -> bool {
        self.area.w < self.config.phone_below
    }

    const fn fresh(&mut self) -> u64 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }

    const fn pane_id(&mut self) -> PaneId {
        PaneId::of(self.fresh())
    }

    const fn tab_id(&mut self) -> TabId {
        TabId(self.fresh())
    }

    // ----- reading ---------------------------------------------------------------------------

    /// Every project, in the order they came.
    #[must_use]
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    /// The project of `home`, by index.
    #[must_use]
    pub fn project_of(&self, home: &GroupKey) -> Option<usize> {
        self.projects.iter().position(|p| p.home == *home)
    }

    /// The project on show, by index.
    #[must_use]
    pub const fn shown_index(&self) -> Option<usize> {
        self.shown
    }

    /// The project on show.
    #[must_use]
    pub fn shown_project(&self) -> Option<&Project> {
        self.projects.get(self.shown?)
    }

    /// The tab on show.
    #[must_use]
    pub fn shown_tab(&self) -> Option<&Tab> {
        self.shown_project()?.shown()
    }

    fn shown_tab_mut(&mut self) -> Option<&mut Tab> {
        let project = self.projects.get_mut(self.shown?)?;
        project.tabs.get_mut(project.shown)
    }

    /// The tile with the focus: the shown tile of the focused pane of the tab on show.
    #[must_use]
    pub fn focused(&self) -> Option<TileRef> {
        self.shown_tab()?.focused()
    }

    /// Every tile, project by project.
    pub fn tiles(&self) -> impl Iterator<Item = TileRef> + '_ {
        self.projects.iter().flat_map(|p| &p.tabs).flat_map(Tab::tiles)
    }

    /// Whether `tile` is placed.
    #[must_use]
    pub fn contains(&self, tile: TileRef) -> bool {
        self.position(tile).is_some()
    }

    /// Where `tile` is.
    #[must_use]
    pub fn position(&self, tile: TileRef) -> Option<Pos> {
        self.projects.iter().enumerate().find_map(|(project, p)| {
            p.tabs
                .iter()
                .find_map(|t| t.pane_of(tile).map(|pane| Pos { project, tab: t.id(), pane }))
        })
    }

    /// Where tab `id` is: its project and its index there.
    #[must_use]
    pub fn tab_place(&self, id: TabId) -> Option<(usize, usize)> {
        self.projects
            .iter()
            .enumerate()
            .find_map(|(p, project)| project.tabs.iter().position(|t| t.id() == id).map(|t| (p, t)))
    }

    /// Whether `tile` shows: its pane takes room in the tab on show and shows it. A tile in a
    /// background tab, a pane put away, behind another of its pane's tabs, or in a project not
    /// on show does not.
    #[must_use]
    pub fn on_show(&self, tile: TileRef) -> bool {
        let frame = self.frame();
        let Some(tab) = self.shown_tab() else { return false };
        frame.panes.iter().any(|l| tab.pane(l.pane).and_then(Pane::shown) == Some(tile))
    }

    /// The tab on show, laid out in the area: on a phone, its focused pane alone over it all.
    #[must_use]
    pub fn frame(&self) -> TabFrame {
        let Some(tab) = self.shown_tab() else { return TabFrame::default() };
        if self.is_phone() {
            let laid = super::tree::Laid { pane: tab.focus(), rect: self.area };
            return TabFrame { panes: vec![laid], sashes: Vec::new() };
        }
        tab.frame(self.area)
    }

    // ----- where new work opens ----------------------------------------------------------

    fn project_or_add(&mut self, home: &GroupKey) -> usize {
        if let Some(i) = self.project_of(home) {
            return i;
        }
        self.projects.push(Project::new(home.clone()));
        self.projects.len().saturating_sub(1)
    }

    /// `tile` in a new tab of `home`'s project, after the tab on show there; the project
    /// shows it, focused (a start, a terminal: ⌘T, ⌘⇧T).
    pub fn new_tab(&mut self, tile: TileRef, home: &GroupKey) -> TabId {
        if let Some(pos) = self.position(tile) {
            self.focus(tile);
            return pos.tab;
        }
        let p = self.project_or_add(home);
        let (pane, id) = (self.pane_id(), self.tab_id());
        let tab = Tab::new(id, Pane::new(pane, tile));
        if let Some(project) = self.projects.get_mut(p) {
            let at = if project.tabs.is_empty() { 0 } else { project.shown.saturating_add(1) };
            project.tabs.insert(at, tab);
            project.shown = at;
        }
        self.show_index(p);
        id
    }

    /// `tile` opened from the focused tile, by the room rule beside it; a new tab of `home`'s
    /// project when nothing is on show. On a phone, a tab of the focused pane.
    pub fn open_beside(&mut self, tile: TileRef, home: &GroupKey) {
        if self.contains(tile) {
            self.focus(tile);
            return;
        }
        let Some(tab) = self.shown_tab() else {
            self.new_tab(tile, home);
            return;
        };
        let (area, room, source) = (self.area, self.config.room, tab.focus());
        if self.is_phone() {
            if let Some(tab) = self.shown_tab_mut() {
                tab.join(source, tile);
            }
            return;
        }
        let frame = tab.frame(area);
        let Some(rect) = frame.rect(source) else { return };
        let right = tab.neighbour(source, Side::Right, area);
        // The row the source stands in: its split when that is a row, else the source alone.
        let path = tab.root().path_of(source).unwrap_or_default();
        let row = path.split_last().and_then(|(_, up)| match tab.root().at(up) {
            Some(Node::Split(s)) if s.axis() == SplitAxis::Row => {
                let len = s.children().len();
                let width = frame
                    .panes
                    .iter()
                    .filter(|l| tab.root().path_of(l.pane).is_some_and(|p| p.starts_with(up)))
                    .map(|l| l.rect)
                    .fold(None::<(f32, f32)>, |acc, r| {
                        Some(acc.map_or_else(
                            || (r.x, r.right()),
                            |(a, b)| (a.min(r.x), b.max(r.right())),
                        ))
                    })
                    .map_or(rect.w, |(a, b)| b - a);
                Some((width, len))
            }
            _ => None,
        });
        let (width, len) = row.unwrap_or((rect.w, 1));
        let id = self.pane_id();
        let Some(tab) = self.shown_tab_mut() else { return };
        if let Some(beside) = right {
            tab.join(beside, tile);
        } else if width / count(len.saturating_add(1)) >= room.min_w {
            tab.split(source, Side::Right, id, tile);
        } else if rect.h / 2.0 >= room.min_h {
            tab.split(source, Side::Bottom, id, tile);
        } else {
            tab.join(source, tile);
        }
    }

    /// `tile` in a new pane on `side` of the focused one (⌘D, ⌘⇧D); a new tab of `home`'s
    /// project when nothing is on show, and a tab of the focused pane on a phone.
    pub fn split_focused(&mut self, tile: TileRef, side: Side, home: &GroupKey) {
        if self.contains(tile) {
            return;
        }
        let phone = self.is_phone();
        let id = self.pane_id();
        let Some(tab) = self.shown_tab_mut() else {
            self.new_tab(tile, home);
            return;
        };
        let source = tab.focus();
        if phone {
            tab.join(source, tile);
        } else {
            tab.split(source, side, id, tile);
        }
    }

    /// `tile` came from elsewhere (another device, the worker's own list): a background tab at
    /// the end of `home`'s project, the focus where it was. With nothing on show, its project
    /// shows.
    pub fn arrive(&mut self, tile: TileRef, home: &GroupKey) {
        if self.contains(tile) {
            return;
        }
        let p = self.project_or_add(home);
        let (pane, id) = (self.pane_id(), self.tab_id());
        if let Some(project) = self.projects.get_mut(p) {
            project.tabs.push(Tab::new(id, Pane::new(pane, tile)));
        }
        if self.shown.is_none() {
            self.show_index(p);
        }
    }

    /// Make `tile` the tab's terminal (⌘⌥T's first press): a pane below the whole tab, a third
    /// of its height, focused. Nothing when nothing is on show or it is placed already.
    pub fn set_terminal(&mut self, tile: TileRef) -> bool {
        if self.contains(tile) {
            return false;
        }
        let id = self.pane_id();
        let Some(tab) = self.shown_tab_mut() else { return false };
        tab.add_terminal(id, tile, TERMINAL_SHARE);
        true
    }

    /// ⌘⌥T once the tab has a terminal: put it away or bring it back, its shells going on.
    /// Whether it shows now; `None` when the tab on show has none yet.
    pub fn toggle_terminal(&mut self) -> Option<bool> {
        let tab = self.shown_tab_mut()?;
        let id = tab.terminal()?;
        let hidden = tab.pane(id)?.hidden();
        tab.set_hidden(id, !hidden).then_some(hidden)
    }

    // ----- taking out --------------------------------------------------------------------

    /// Take `tile` out. A pane, a tab and the tree close up behind it; a tab left empty goes,
    /// and its project shows the tab before it.
    pub fn remove(&mut self, tile: TileRef) {
        let Some(pos) = self.position(tile) else { return };
        let Some(project) = self.projects.get_mut(pos.project) else { return };
        let Some(at) = project.tabs.iter().position(|t| t.id() == pos.tab) else { return };
        let taken = project.tabs.get_mut(at).map_or(Taken::Absent, |t| t.take(tile));
        if taken == Taken::Emptied {
            project.tabs.remove(at);
            if project.shown > at || project.shown >= project.tabs.len() {
                project.shown = project.shown.saturating_sub(1);
            }
            self.forget_visits(pos.tab);
        }
    }

    /// Take out `worker`'s tiles whose item fails `keep` (its first snapshot after a link).
    pub fn retain_worker(&mut self, worker: WorkerKey, keep: impl Fn(ItemId) -> bool) {
        let gone: Vec<TileRef> =
            self.tiles().filter(|t| t.worker == worker && !keep(t.item)).collect();
        for tile in gone {
            self.remove(tile);
        }
    }

    /// Take tab `id` out whole, its tiles with it (a title tab's close). Its tiles, for the
    /// caller to close.
    pub fn drop_tab(&mut self, id: TabId) -> Vec<TileRef> {
        let Some((p, at)) = self.tab_place(id) else { return Vec::new() };
        let Some(project) = self.projects.get_mut(p) else { return Vec::new() };
        let tab = project.tabs.remove(at);
        if project.shown > at || project.shown >= project.tabs.len() {
            project.shown = project.shown.saturating_sub(1);
        }
        self.forget_visits(id);
        tab.tiles().collect()
    }

    // ----- going places ------------------------------------------------------------------

    /// Show project `p` on the tab it was left on. A project left with no tab and no name
    /// goes.
    fn show_index(&mut self, p: usize) {
        let mut p = p;
        if let Some(left) = self.shown.filter(|s| *s != p)
            && self.projects.get(left).is_some_and(|l| l.tabs.is_empty() && l.name.is_none())
        {
            self.projects.remove(left);
            if p > left {
                p = p.saturating_sub(1);
            }
        }
        if p >= self.projects.len() {
            self.shown = None;
            return;
        }
        self.shown = Some(p);
        if let Some(tab) = self.shown_tab().map(Tab::id) {
            self.visit(tab);
        }
    }

    /// Show `home`'s project on the tab it was left on; an unknown one comes to be, empty,
    /// for its first tab.
    pub fn show_project(&mut self, home: &GroupKey) {
        let p = self.project_or_add(home);
        self.show_index(p);
    }

    /// Show the project before or after the one on show, in their order, round the ends.
    pub fn step_project(&mut self, forward: bool) {
        let n = self.projects.len();
        let Some(at) = self.shown.filter(|_| n > 1) else { return };
        let p = super::tree::round_step(at, n, forward);
        self.show_index(p);
    }

    /// Give the project of `home` the person's name for it, or none.
    pub fn set_name(&mut self, home: &GroupKey, name: Option<String>) {
        let p = self.project_or_add(home);
        if let Some(project) = self.projects.get_mut(p) {
            project.name = name.filter(|n| !n.trim().is_empty());
        }
    }

    /// Show the project's tab `n` (⌘1…⌘8); past the end, nothing.
    pub fn select_tab(&mut self, n: usize) {
        let Some(project) = self.shown.and_then(|p| self.projects.get_mut(p)) else { return };
        if n < project.tabs.len() {
            project.shown = n;
            if let Some(id) = project.tabs.get(n).map(Tab::id) {
                self.visit(id);
            }
        }
    }

    /// Show the project's last tab (⌘9).
    pub fn last_tab(&mut self) {
        let n = self.shown_project().map_or(0, |p| p.tabs.len());
        if n > 0 {
            self.select_tab(n.saturating_sub(1));
        }
    }

    /// Show the tab before or after the one on show, round the ends (⌘⇧[ ⌘⇧]).
    pub fn step_tab(&mut self, forward: bool) {
        let Some(project) = self.shown_project() else { return };
        let n = project.tabs.len();
        if n > 1 {
            let at = project.shown;
            self.select_tab(super::tree::round_step(at, n, forward));
        }
    }

    /// Show tab `id`, wherever it is.
    pub fn show_tab(&mut self, id: TabId) {
        let Some((p, t)) = self.tab_place(id) else { return };
        if let Some(project) = self.projects.get_mut(p) {
            project.shown = t;
        }
        self.show_index(p);
    }

    /// Focus `tile`: its project and tab show, and its pane shows it.
    pub fn focus(&mut self, tile: TileRef) {
        let Some(pos) = self.position(tile) else { return };
        if let Some(project) = self.projects.get_mut(pos.project) {
            if let Some(t) = project.tabs.iter().position(|t| t.id() == pos.tab) {
                project.shown = t;
            }
            if let Some(tab) = project.tab_mut(pos.tab) {
                tab.focus_pane(pos.pane, Some(tile));
            }
        }
        self.show_index(pos.project);
    }

    /// Tab `id` closed: its visits go, and back and forward stand where they stood.
    fn forget_visits(&mut self, id: TabId) {
        let before = self.visits.iter().take(self.at).filter(|v| **v != id).count();
        let stays = self.visits.get(self.at).is_some_and(|v| *v != id);
        self.visits.retain(|v| *v != id);
        let at = if stays { before } else { before.saturating_sub(1) };
        self.at = at.min(self.visits.len().saturating_sub(1));
    }

    /// Record a visit to tab `id`: the visits after where back stood go.
    fn visit(&mut self, id: TabId) {
        if self.visits.get(self.at) == Some(&id) {
            return;
        }
        self.visits.truncate(self.at.saturating_add(1).min(self.visits.len()));
        self.visits.push(id);
        if self.visits.len() > VISITS_KEPT {
            self.visits.remove(0);
        }
        self.at = self.visits.len().saturating_sub(1);
    }

    /// Back or forward through the tabs visited (⌘[ ⌘]), past those since closed.
    pub fn go_back(&mut self, forward: bool) -> bool {
        let mut at = self.at;
        loop {
            let next = if forward { at.checked_add(1) } else { at.checked_sub(1) };
            let Some(next) = next.filter(|n| *n < self.visits.len()) else { return false };
            at = next;
            let Some(id) = self.visits.get(at).copied() else { return false };
            // A visit to the tab on show, or to one since closed, is passed over.
            if Some(id) == self.shown_tab().map(Tab::id) {
                continue;
            }
            if let Some((p, t)) = self.tab_place(id) {
                self.at = at;
                if let Some(project) = self.projects.get_mut(p) {
                    project.shown = t;
                }
                // Not a visit of its own: the trail stays as it was.
                self.shown = Some(p);
                return true;
            }
        }
    }

    // ----- in the tab on show --------------------------------------------------------------

    /// Focus the pane on `side` of the focused one (⌘⌥ and an arrow).
    pub fn focus_side(&mut self, side: Side) -> bool {
        let area = self.area;
        let Some(tab) = self.shown_tab_mut() else { return false };
        let Some(to) = tab.neighbour(tab.focus(), side, area) else { return false };
        tab.focus_pane(to, None)
    }

    /// Move the focused tile toward `side` (⌘⌥⇧ and an arrow): into the pane there, else out
    /// along the tab's edge.
    pub fn move_focused(&mut self, side: Side) -> bool {
        let area = self.area;
        let id = self.pane_id();
        let Some(tab) = self.shown_tab_mut() else { return false };
        let Some(tile) = tab.focused() else { return false };
        tab.move_tile(tile, side, area, id)
    }

    /// Show the focused pane's tab before or after the one it shows (⌘⌥[ ⌘⌥]).
    pub fn step_pane_tab(&mut self, forward: bool) {
        let Some(tab) = self.shown_tab_mut() else { return };
        let focus = tab.focus();
        if let Some(pane) = tab.pane_mut(focus) {
            pane.step(forward);
        }
    }

    /// Zoom the focused pane over the tab, or let it go (⇧⌘↩). Whether it is zoomed now.
    pub fn toggle_zoom(&mut self) -> bool {
        self.shown_tab_mut().is_some_and(Tab::toggle_zoom)
    }

    /// Drag `sash` of the tab on show by `delta` points; how far it went.
    pub fn drag_sash(&mut self, sash: &super::tree::Sash, delta: f32) -> f32 {
        let (area, room) = (self.area, self.config.room);
        self.shown_tab_mut().map_or(0.0, |tab| tab.drag_sash(sash, delta, area, room))
    }

    /// Make the shares of the split at `path` equal (a double-click on its sash).
    pub fn equalize(&mut self, path: &[usize]) -> bool {
        self.shown_tab_mut().is_some_and(|tab| tab.equalize(path))
    }

    /// Make every split's shares equal ("Equalize panes").
    pub fn equalize_all(&mut self) {
        if let Some(tab) = self.shown_tab_mut() {
            tab.equalize_all();
        }
    }

    // ----- dragging ----------------------------------------------------------------------

    /// Where a tile dropped at `(x, y)` in the area lands: within [`DROP_BAND`] of a pane's
    /// shorter side from its nearest edge it splits there, and elsewhere it joins the pane's
    /// tabs (Zed's band). On a phone, every drop joins.
    #[must_use]
    pub fn drop_target(&self, x: f32, y: f32) -> Option<Drop> {
        let frame = self.frame();
        let laid = frame.panes.iter().find(|l| l.rect.contains(x, y))?;
        let r = laid.rect;
        if self.is_phone() {
            return Some(Drop { pane: laid.pane, edge: None });
        }
        let band = DROP_BAND * r.w.min(r.h);
        let edges = [
            (x - r.x, Side::Left),
            (r.right() - x, Side::Right),
            (y - r.y, Side::Top),
            (r.bottom() - y, Side::Bottom),
        ];
        let (gap, side) = edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0))?;
        Some(Drop { pane: laid.pane, edge: (gap < band).then_some(side) })
    }

    /// Put `tile`, from wherever it is, where `drop` says in the tab on show. A tile dropped
    /// on its own pane alone stays.
    pub fn place(&mut self, tile: TileRef, drop: Drop) -> bool {
        let tab_on_show = self.shown_tab().map(Tab::id);
        let Some(tab) = self.shown_tab() else { return false };
        let target = tab.pane(drop.pane);
        let Some(target) = target else { return false };
        let alone_there = target.tiles() == [tile];
        if alone_there {
            return false;
        }
        if drop.edge.is_none() && target.tiles().contains(&tile) {
            return false;
        }
        // Taking it out may close panes and tabs, never the drop's pane, which holds another.
        self.remove(tile);
        let id = self.pane_id();
        let Some(tab) = self.shown_tab_mut().filter(|t| Some(t.id()) == tab_on_show) else {
            return false;
        };
        match drop.edge {
            None => tab.join(drop.pane, tile),
            Some(side) => tab.split(drop.pane, side, id, tile),
        }
    }

    /// Put `tile` back where it stood before it closed, `at` (its close taken back): a tab of
    /// its pane when that is still there, else a new pane right of its tab's focused one; the
    /// focus goes with it. Nothing when its tab has gone or holds it already.
    pub fn put_back(&mut self, tile: TileRef, at: Pos) -> bool {
        let Some((p, t)) = self.tab_place(at.tab) else { return false };
        let holds = self.projects.get(p).and_then(|pr| pr.tabs.get(t)).map(|tab| tab.pane_of(tile));
        if holds.flatten().is_some() {
            return false;
        }
        self.remove(tile);
        let id = self.pane_id();
        let Some(project) = self.projects.get_mut(p) else { return false };
        let Some(tab) = project.tab_mut(at.tab) else { return false };
        let placed = if tab.pane(at.pane).is_some() {
            tab.join(at.pane, tile)
        } else {
            let focus = tab.focus();
            tab.split(focus, Side::Right, id, tile)
        };
        if placed {
            self.focus(tile);
        }
        placed
    }

    /// Put `tile` in a new tab of `home`'s project, after its shown one, on purpose ("Move to
    /// project…", a drop on a project's row, or on the title strip); the focus goes with it.
    pub fn move_to_project(&mut self, tile: TileRef, home: &GroupKey) {
        self.remove(tile);
        self.new_tab(tile, home);
    }

    // ----- keeping -----------------------------------------------------------------------

    /// What a relaunch begins from.
    #[must_use]
    pub fn save(&self) -> SavedTiling {
        let index = |id: TabId| self.tab_place(id);
        SavedTiling {
            projects: self.projects.iter().map(save_project).collect(),
            shown: self.shown,
            visits: self.visits.iter().filter_map(|v| index(*v)).collect(),
        }
    }

    /// A workspace from what [`Self::save`] kept, cleaned: every tree in its normal form,
    /// panes and tabs left empty dropped, indices clamped.
    #[must_use]
    pub fn restore(saved: SavedTiling, config: TilingConfig) -> Self {
        let mut tiling = Self::new(config);
        let mut seen: Vec<TileRef> = Vec::new();
        // Each saved project's new index, and its saved tabs' new ids.
        let mut index: Vec<Option<usize>> = Vec::new();
        let mut ids: Vec<Vec<Option<TabId>>> = Vec::new();
        for project in saved.projects {
            let mut out = Project::new(project.home);
            out.name = project.name;
            let mut tab_ids = Vec::new();
            for saved_tab in project.tabs {
                let id = tiling.tab_id();
                match tiling.restore_tab(id, saved_tab, &mut seen) {
                    Some(tab) => {
                        tab_ids.push(Some(id));
                        out.tabs.push(tab);
                    }
                    None => tab_ids.push(None),
                }
            }
            // The tab it was left on, counted among those kept.
            let kept_before = tab_ids.iter().take(project.shown).filter(|t| t.is_some()).count();
            out.shown = kept_before.min(out.tabs.len().saturating_sub(1));
            ids.push(tab_ids);
            if out.tabs.is_empty() && out.name.is_none() {
                index.push(None);
                continue;
            }
            index.push(Some(tiling.projects.len()));
            tiling.projects.push(out);
        }
        tiling.shown = saved
            .shown
            .and_then(|s| index.get(s).copied().flatten())
            .or_else(|| (!tiling.projects.is_empty()).then_some(0));
        tiling.visits = saved
            .visits
            .iter()
            .filter_map(|(p, t)| ids.get(*p)?.get(*t).copied().flatten())
            .collect();
        tiling.visits.dedup();
        tiling.at = tiling.visits.len().saturating_sub(1);
        tiling
    }

    fn restore_tab(&mut self, id: TabId, saved: SavedTab, seen: &mut Vec<TileRef>) -> Option<Tab> {
        let root = self.restore_node(saved.root, seen)?;
        Tab::restored(id, root, &saved.focus, saved.zoomed, saved.terminal.as_deref())
    }

    fn restore_node(&mut self, saved: SavedNode, seen: &mut Vec<TileRef>) -> Option<Node> {
        match saved {
            SavedNode::Pane { tiles, shown, hidden } => {
                let tiles: Vec<TileRef> = tiles
                    .into_iter()
                    .filter(|t| {
                        let fresh = !seen.contains(t);
                        seen.push(*t);
                        fresh
                    })
                    .collect();
                let pane = Pane::restored(self.pane_id(), tiles, shown, hidden)?;
                Some(Node::Pane(pane))
            }
            SavedNode::Split { axis, children, shares } => {
                let mut kids = Vec::new();
                let mut parts = Vec::new();
                for (i, child) in children.into_iter().enumerate() {
                    if let Some(node) = self.restore_node(child, seen) {
                        kids.push(node);
                        parts.push(shares.get(i).copied().unwrap_or(0.0));
                    }
                }
                Some(Node::Split(Split::of(axis, kids, parts)))
            }
        }
    }
}

fn save_project(project: &Project) -> SavedProject {
    SavedProject {
        home: project.home.clone(),
        name: project.name.clone(),
        tabs: project.tabs.iter().map(save_tab).collect(),
        shown: project.shown,
    }
}

fn save_tab(tab: &Tab) -> SavedTab {
    let path = |id: PaneId| tab.root().path_of(id).unwrap_or_default();
    SavedTab {
        root: save_node(tab.root()),
        focus: path(tab.focus()),
        zoomed: tab.zoomed().is_some(),
        terminal: tab.terminal().map(path),
    }
}

fn save_node(node: &Node) -> SavedNode {
    match node {
        Node::Pane(p) => SavedNode::Pane {
            tiles: p.tiles().to_vec(),
            shown: p.shown_index(),
            hidden: p.hidden(),
        },
        Node::Split(s) => SavedNode::Split {
            axis: s.axis(),
            children: s.children().iter().map(save_node).collect(),
            shares: s.shares().to_vec(),
        },
    }
}

/// The saved form of the workspace.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct SavedTiling {
    /// Every project.
    pub projects: Vec<SavedProject>,
    /// The one on show.
    pub shown: Option<usize>,
    /// The tabs visited, oldest first, as (project, tab).
    pub visits: Vec<(usize, usize)>,
}

/// A project as saved.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SavedProject {
    /// Its group.
    pub home: GroupKey,
    /// The person's name for it.
    pub name: Option<String>,
    /// Its tabs.
    pub tabs: Vec<SavedTab>,
    /// The tab it was left on.
    pub shown: usize,
}

/// A tab as saved: its tree, and its panes named by path.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SavedTab {
    /// Its tree.
    pub root: SavedNode,
    /// The focused pane's path.
    pub focus: Vec<usize>,
    /// The focused pane was zoomed.
    pub zoomed: bool,
    /// The tab's terminal's path.
    pub terminal: Option<Vec<usize>>,
}

/// A node as saved.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum SavedNode {
    /// An inner node.
    Split {
        /// Its axis.
        axis: SplitAxis,
        /// Its children.
        children: Vec<Self>,
        /// Their shares.
        shares: Vec<f32>,
    },
    /// A leaf.
    Pane {
        /// Its tiles.
        tiles: Vec<TileRef>,
        /// The one shown.
        shown: usize,
        /// Put away.
        hidden: bool,
    },
}

#[expect(clippy::cast_precision_loss, reason = "counts of panes are tiny")]
const fn count(n: usize) -> f32 {
    n as f32
}

#[cfg(test)]
mod tests;
