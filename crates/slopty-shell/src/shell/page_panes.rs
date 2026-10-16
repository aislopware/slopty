//! slopty: a page shown in the right pane as a tab ([`RightSurface::Page`]): a card's "Open in
//! pane" asks through [`PagePaneAsk`], and the tab opens in front, the pane open; "Bring back"
//! (or closing the tab) puts the page back in its card. Each piece is a call from the shell's own
//! match on the surface, as `mod_panes` does for a mod's pane.

use std::collections::HashMap;

use gpui::{AnyElement, App, AppContext as _, Context, Entity, Global, IntoElement, SharedString};

use super::{RightSurface, Shell};
use crate::page::{PagePaneAsk, PagePaneView};

/// The page panes, by their tab's number.
#[derive(Default)]
struct PagePanes {
    seq: u64,
    tabs: HashMap<u64, Entity<PagePaneView>>,
}

impl Global for PagePanes {}

impl Shell {
    /// A card asked for its page in the pane, or back out of it.
    pub(super) fn on_page_pane_ask(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = cx.try_global::<PagePaneAsk>().cloned() else { return };
        let key = self.panel_key(cx);
        let shown = cx.try_global::<PagePanes>().and_then(|panes| {
            panes
                .tabs
                .iter()
                .find(|(_, pane)| pane.read(cx).page() == ask.page)
                .map(|(id, _)| *id)
        });
        match (ask.show, shown) {
            (true, Some(id)) => {
                self.set_surfaces_open(true, cx);
                self.set_right_active(RightSurface::Page(id), cx);
            }
            (true, None) => {
                let engine = self.state.read(cx).engine().cloned();
                let pane =
                    cx.new(|cx| PagePaneView::new(ask.page, ask.title, ask.chat, engine, cx));
                let panes = cx.default_global::<PagePanes>();
                panes.seq += 1;
                let id = panes.seq;
                panes.tabs.insert(id, pane);
                self.right_tabs.entry(key).or_default().push(RightSurface::Page(id));
                self.set_surfaces_open(true, cx);
                self.set_right_active(RightSurface::Page(id), cx);
            }
            (false, Some(id)) => {
                let surface = RightSurface::Page(id);
                for tabs in self.right_tabs.values_mut() {
                    tabs.retain(|s| *s != surface);
                }
                self.panels.forget_right_surface(&key, surface);
                self.close_page_pane(id, cx);
                self.close_empty_right_pane(&key, cx);
            }
            (false, None) => {}
        }
        cx.notify();
    }

    /// The tab's chip: the page's title.
    pub(super) fn page_pane_row(
        &self,
        id: u64,
        cx: &App,
    ) -> Option<(RightSurface, SharedString, bool, Option<SharedString>)> {
        let pane = cx.try_global::<PagePanes>()?.tabs.get(&id)?;
        Some((RightSurface::Page(id), pane.read(cx).title(), false, None))
    }

    /// The pane's body, or the picker once it is gone.
    pub(super) fn render_page_pane(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        match cx.try_global::<PagePanes>().and_then(|p| p.tabs.get(&id)).cloned() {
            Some(pane) => pane.into_any_element(),
            None => self.render_surface_picker(cx),
        }
    }

    /// The tab closed: the page goes back to its card.
    pub(super) fn close_page_pane(&mut self, id: u64, cx: &mut Context<Self>) {
        if cx.has_global::<PagePanes>() {
            cx.global_mut::<PagePanes>().tabs.remove(&id);
        }
    }
}
