//! Going to a project: the palette's line for each, ranked by how often and how lately this
//! device went to it; where ↩ lands (the workspace that last held its tiles); and the scope that
//! narrows the frame to one project.
//!
//! The ranking is zoxide's ([`slopty_client::groups::Frecency`]), kept with the layout, and
//! counts a visit each time the focus comes to a tile of another project than the last. It
//! ranks only the palette's projects, a transient switcher: the navigator's groups and the
//! panes keep their places.

use gpui::{Context, Window};
use slopty_client::groups::{GroupKey, fact};
use slopty_client::layout::TileRef;
use slopty_core::WallMs;
use slopty_proto::items::ItemOp;

use super::WorkspaceView;
use super::actions::{PinToProject, ScopeTo};
use super::grouping::group_glyph;
use super::rollup::Rollup;
use crate::icons::Status;
use crate::palette::PaletteItem;

/// What the palette calls letting go of a scope.
pub(super) const CLEAR_SCOPE: &str = "Clear the scope";

/// The key of an item's fact that pins it to a project: the key of the group it joins.
pub(super) const PIN: &str = fact::PROJECT;

/// What each project's line says it holds: its agents, else its tiles.
fn holds(agents: usize, tiles: usize) -> String {
    match (agents, tiles) {
        (1, _) => "1 agent".to_owned(),
        (0, 1) => "1 tile".to_owned(),
        (0, n) => format!("{n} tiles"),
        (n, _) => format!("{n} agents"),
    }
}

impl WorkspaceView {
    /// The palette's line for each project: its name, how it stands, the machines it spans
    /// and what it holds; the ones gone to most and latest first. ↩ goes to it.
    pub(super) fn project_rows(&self) -> Vec<PaletteItem> {
        let grouping = self.project_groups();
        let now = WallMs::now().as_millis();
        let mut rows: Vec<(f64, String, PaletteItem)> = grouping
            .grouped
            .groups
            .iter()
            .filter(|g| g.key.worker().is_none())
            .map(|group| {
                let mut rollup = Rollup::default();
                let (mut agents, mut tiles) = (0_usize, 0_usize);
                for tile in grouping.members(group) {
                    let Some(item) = self.item(tile) else { continue };
                    let (mark, unseen) = self.tile_marks(tile, item);
                    rollup.add(mark, unseen);
                    tiles = tiles.saturating_add(1);
                    if self.item_agent(item).is_some() {
                        agents = agents.saturating_add(1);
                    }
                }
                let status = match rollup.shown() {
                    Some(super::rollup::Shown::NeedsYou(_)) => Some(Status::NeedsYou),
                    Some(super::rollup::Shown::Working) => Some(Status::Working),
                    _ => None,
                };
                let name = self.group_name(group);
                let line = PaletteItem::group(&name, group_glyph(&group.fact), group.key.clone())
                    .with_status(status)
                    .on_worker(self.machines_word(&grouping.machines(group)))
                    .placed(Some(holds(agents, tiles)));
                (self.frecency.score(&group.key, now), name.to_lowercase(), line)
            })
            .collect();
        rows.sort_by(|(a, an, _), (b, bn, _)| b.total_cmp(a).then_with(|| an.cmp(bn)));
        rows.into_iter().map(|(_, _, line)| line).collect()
    }

    /// "Scope to `project`" for each project, and the way back out while one is scoped.
    pub(super) fn scope_lines(&self) -> Vec<PaletteItem> {
        let grouping = self.project_groups();
        let mut lines: Vec<PaletteItem> = grouping
            .grouped
            .groups
            .iter()
            .filter(|g| g.key.worker().is_none() && self.nav.scope.as_ref() != Some(&g.key))
            .map(|group| {
                let label = format!("Scope to {}", self.group_name(group));
                let action = ScopeTo { project: Some(group.key.clone()) };
                PaletteItem::new(&label, Box::new(action), &[])
            })
            .collect();
        if self.nav.scope.is_some() {
            let action = ScopeTo { project: None };
            lines.push(PaletteItem::new(CLEAR_SCOPE, Box::new(action), &[]));
        }
        lines
    }

    /// For the focused tile: "Add to `project`" for each project it is not in, and "Take out
    /// of `project`" for the one it is pinned to.
    pub(super) fn pin_lines(&self) -> Vec<PaletteItem> {
        let Some(tile) = self.focused() else { return Vec::new() };
        let Some(item) = self.item(tile) else { return Vec::new() };
        let grouping = self.project_groups();
        let own = grouping.group_of(tile);
        let mut lines = Vec::new();
        if let Some(pinned) = item.facts.get(PIN).and_then(|v| GroupKey::parse(v)) {
            // The pin put the tile in that group, whose key may be a stronger value since.
            let name = grouping
                .group(&pinned)
                .or(own)
                .map_or_else(|| pinned.value().to_owned(), |g| self.group_name(g));
            let action = PinToProject { project: None };
            lines.push(PaletteItem::new(&format!("Take out of {name}"), Box::new(action), &[]));
        }
        lines.extend(
            grouping
                .grouped
                .groups
                .iter()
                .filter(|g| g.key.worker().is_none() && own.is_none_or(|o| o.key != g.key))
                .map(|group| {
                    let label = format!("Add to {}", self.group_name(group));
                    let action = PinToProject { project: Some(group.key.clone()) };
                    PaletteItem::new(&label, Box::new(action), &[])
                }),
        );
        lines
    }

    /// "Add to …" or "Take out of …" from the palette: the pin is said to the focused tile's
    /// worker, which keeps it with the item for every client.
    pub(super) fn pin_to_project(
        &mut self,
        pin: &PinToProject,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.focused() else { return };
        let value = pin.project.as_ref().map(|key| key.as_str().to_owned());
        let op = ItemOp::SetFact { id: tile.item, key: PIN.to_owned(), value };
        self.propose(tile.worker, op, cx);
        cx.notify();
    }

    /// "Scope to …" from the palette.
    pub(super) fn scope_to(
        &mut self,
        scope: &ScopeTo,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_scope(scope.project.clone(), cx);
    }

    /// Narrow the frame to `scope`'s project, or let go of it.
    pub(super) fn set_scope(&mut self, scope: Option<GroupKey>, cx: &mut Context<Self>) {
        if self.nav.scope != scope {
            self.nav.scope = scope;
            self.changed(cx);
            cx.notify();
        }
    }

    /// The project the frame is narrowed to, by name.
    pub(super) fn scope_name(&self) -> Option<String> {
        let scope = self.nav.scope.as_ref()?;
        let grouping = self.project_groups();
        Some(grouping.group(scope).map_or_else(|| scope.value().to_owned(), |g| self.group_name(g)))
    }

    /// Go to `group`: its project, on the tab it was left on. A group whose work sits in
    /// another project's tabs (a shell opened beside other work) goes to its first tile in
    /// reading order there. A group with no tile in the tiling says so.
    pub(super) fn go_to_group(&mut self, group: &GroupKey, cx: &mut Context<Self>) {
        if !self.reach_group(group, cx) {
            self.say_no_tile(group, cx);
        }
    }

    /// `group` has no tile in the tiling: say so.
    pub(super) fn say_no_tile(&mut self, group: &GroupKey, cx: &mut Context<Self>) {
        self.show_notice(format!("{} has no tile here", group.value()), cx);
    }

    /// [`Self::go_to_group`] but for the notice: whether `group` had a tile to go to.
    pub(super) fn reach_group(&mut self, group: &GroupKey, cx: &mut Context<Self>) -> bool {
        let project = self.layout.project_of(group).and_then(|ix| self.layout.projects().get(ix));
        if project.is_some_and(|p| !p.tabs().is_empty()) {
            let group = group.clone();
            self.layout_action(cx, |l| l.show_project(&group));
            if let Some(tile) = self.focused() {
                self.navigated_to(tile);
            }
            return true;
        }
        let grouping = self.project_groups();
        let first = self
            .reading_order()
            .into_iter()
            .find(|t| grouping.group_of(*t).is_some_and(|g| &g.key == group));
        let Some(tile) = first else { return false };
        self.navigated_to(tile);
        self.focus_tile(tile, cx);
        true
    }

    /// The focus came to `tile`: a visit to its project when that is not the one gone to last.
    pub(super) fn navigated_to(&mut self, tile: TileRef) {
        let home = self.home_for(tile);
        if home.worker().is_some() || self.last_project.as_ref() == Some(&home) {
            return;
        }
        self.frecency.visit(&home, WallMs::now().as_millis());
        self.last_project = Some(home);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project's line says its agents, else its tiles, in words that agree in number.
    #[test]
    fn a_project_line_says_what_it_holds() {
        assert_eq!(holds(0, 1), "1 tile");
        assert_eq!(holds(0, 4), "4 tiles");
        assert_eq!(holds(1, 4), "1 agent");
        assert_eq!(holds(3, 4), "3 agents");
        assert!(CLEAR_SCOPE.chars().next().is_some_and(char::is_uppercase), "sentence case");
    }
}
