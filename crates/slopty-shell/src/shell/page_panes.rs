//! A page an agent showed, as a view beside the session ([`RightSurface::Page`]): a card's
//! "Open in pane" asks through [`PagePaneAsk`], and the view docks beside the session with the
//! keyboard; "Bring back" (or closing the view) puts the page back in its card. Each piece is a
//! call from the shell's own match on the view.

use std::collections::HashMap;

use gpui::{AnyElement, App, AppContext as _, Context, Empty, Entity, Global, IntoElement, SharedString};

use super::{RightSurface, Shell};
use crate::page::{PagePaneAsk, PagePaneView};

/// The page views, by their number.
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
        let shown = cx.try_global::<PagePanes>().and_then(|panes| {
            panes
                .tabs
                .iter()
                .find(|(_, pane)| pane.read(cx).page() == ask.page)
                .map(|(id, _)| *id)
        });
        match (ask.show, shown) {
            (true, Some(id)) => self.open_view(RightSurface::Page(id), cx),
            (true, None) => {
                let engine = self.state.read(cx).engine().cloned();
                let pane =
                    cx.new(|cx| PagePaneView::new(ask.page, ask.title, ask.chat, engine, cx));
                let panes = cx.default_global::<PagePanes>();
                panes.seq += 1;
                let id = panes.seq;
                panes.tabs.insert(id, pane);
                self.open_view(RightSurface::Page(id), cx);
            }
            (false, Some(id)) => {
                // Back in its card: out of whichever session's view holds it.
                let view = RightSurface::Page(id);
                let keys: Vec<String> = self
                    .panes
                    .iter()
                    .filter(|(_, panes)| panes.contains(super::panes::Pane::View(view)))
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in keys {
                    self.drop_view(view, &key, cx);
                }
                self.close_page_pane(id, cx);
            }
            (false, None) => {}
        }
        cx.notify();
    }

    /// The view's title: the page's.
    pub(super) fn page_pane_title(&self, id: u64, cx: &App) -> Option<SharedString> {
        Some(cx.try_global::<PagePanes>()?.tabs.get(&id)?.read(cx).title())
    }

    /// The view's body, nothing once it is gone.
    pub(super) fn render_page_pane(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        match cx.try_global::<PagePanes>().and_then(|p| p.tabs.get(&id)).cloned() {
            Some(pane) => pane.into_any_element(),
            None => Empty.into_any_element(),
        }
    }

    /// The view closed: the page goes back to its card.
    pub(super) fn close_page_pane(&mut self, id: u64, cx: &mut Context<Self>) {
        if cx.has_global::<PagePanes>() {
            cx.global_mut::<PagePanes>().tabs.remove(&id);
        }
    }
}
