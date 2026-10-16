//! slopty: a mod's pane as a right-pane tab ([`RightSurface::ModPane`]): the composer asks for
//! one through [`PaneAsk`] when the chat's dock first shows it, and the tab is added to that
//! chat's pane, there when the pane opens, never opening it unasked. Each piece is a call from
//! the shell's own match on the surface, as `live_surfaces` does for screens.

use std::collections::HashMap;

use gpui::{AnyElement, App, AppContext as _, Context, Entity, Global, IntoElement, SharedString};

use super::{RightSurface, Shell};
use crate::composer::{ModPane, PaneAsk};

/// The mod panes, by their tab's number.
#[derive(Default)]
struct ModPanes {
    seq: u64,
    tabs: HashMap<u64, Entity<ModPane>>,
}

impl Global for ModPanes {}

impl Shell {
    /// The composer asked for a mod's pane: a tab in its chat's pane.
    pub(super) fn on_pane_ask(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = cx.try_global::<PaneAsk>().cloned() else { return };
        let state = self.state.clone();
        let pane = cx.new(|cx| ModPane::new(state, &ask, cx));
        let panes = cx.default_global::<ModPanes>();
        panes.seq += 1;
        let id = panes.seq;
        panes.tabs.insert(id, pane);
        self.right_tabs.entry(ask.chat_id).or_default().push(RightSurface::ModPane(id));
        cx.notify();
    }

    /// The tab's chip: the pane's title.
    pub(super) fn mod_pane_row(
        &self,
        id: u64,
        cx: &App,
    ) -> Option<(RightSurface, SharedString, bool, Option<SharedString>)> {
        let pane = cx.try_global::<ModPanes>()?.tabs.get(&id)?;
        Some((RightSurface::ModPane(id), pane.read(cx).title(), false, None))
    }

    /// The pane's body, or the picker once it is gone.
    pub(super) fn render_mod_pane(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        match cx.try_global::<ModPanes>().and_then(|p| p.tabs.get(&id)).cloned() {
            Some(pane) => pane.into_any_element(),
            None => self.render_surface_picker(cx),
        }
    }

    /// The tab closed.
    pub(super) fn close_mod_pane(&mut self, id: u64, cx: &mut Context<Self>) {
        if cx.has_global::<ModPanes>() {
            cx.global_mut::<ModPanes>().tabs.remove(&id);
        }
    }
}
