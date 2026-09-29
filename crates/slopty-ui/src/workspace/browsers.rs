//! Web pages in tiles: opening one (a forwarded port, an address typed into the palette), a
//! view per browser item, the address field in its header and the page's history, and each
//! frame the native page put over its tile or hidden.

use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Entity, IntoElement as _, Styled as _, Window, canvas};
use gpui_kit::component::input::InputState;
use slopty_client::layout::{Rect, TileRef, WorkerKey};
use slopty_core::ItemId;
use slopty_proto::items::{Item, ItemKind, ItemOp};

use super::actions::{EditAddress, InspectPage, OpenUrl, PageBack, PageForward, ReloadPage};
use super::{Field, WorkspaceView};
use crate::browser::{BrowserEvent, BrowserView, Cover, Zoom};
use crate::draw::Draw;
use crate::palette::CommandPalette;

/// What the "Open URL…" palette starts with: the forwarded ports live on localhost.
const URL_SEED: &str = "http://localhost:";
/// The address field's name, and what it says with nothing typed.
pub(super) const ADDRESS: &str = "Address";

impl WorkspaceView {
    /// A browser tile for `url` on `key` (the context worker when `None`): an existing tile
    /// for the address is focused, else a new one opens right of the focus.
    pub fn open_browser(&mut self, key: Option<WorkerKey>, url: &str, cx: &mut Context<Self>) {
        let Some(key) = key.or_else(|| self.context_worker()) else { return };
        let Some(url) = crate::browser::web_url(url) else {
            self.show_notice(format!("Not a web address: {url}"), cx);
            return;
        };
        let existing = self.workers.get(&key).and_then(|w| {
            w.doc.items().find_map(|i| match &i.kind {
                ItemKind::Browser { url: u } if *u == url => Some(i.id),
                _ => None,
            })
        });
        if let Some(id) = existing {
            self.go_to(id, cx);
            return;
        }
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Browser { url: url.clone() },
            sleeping: false,
            name: None,
        };
        tracing::info!(id = %item.id, %url, "open browser tile");
        self.propose(key, ItemOp::Add(item), cx);
        cx.notify();
    }

    /// "Open URL…": the palette, the field started at `http://localhost:`.
    pub fn open_url_palette(&mut self, _: &OpenUrl, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let theme = self.theme.clone();
        let palette = cx.new(|cx| {
            let mut p = CommandPalette::new(Vec::new(), theme, window, cx);
            p.seed(URL_SEED, window, cx);
            p
        });
        self.show_palette(palette, window, cx);
    }

    /// ⌘L: the focused page's address as a field in its header; with no page focused,
    /// "Open URL…".
    pub fn edit_address(&mut self, _: &EditAddress, window: &mut Window, cx: &mut Context<Self>) {
        match self.focused().filter(|t| self.browsers.contains_key(&t.item)) {
            Some(tile) => self.start_address(tile, window, cx),
            None => self.open_url_palette(&OpenUrl, window, cx),
        }
    }

    /// The header of `tile`, a page, turns into its address, the whole of it selected. A page
    /// that had the keyboard gives it to the field.
    pub(super) fn start_address(
        &mut self,
        tile: TileRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.browsers.get(&tile.item).cloned() else { return };
        let url = {
            let page = view.read(cx);
            if page.focused() {
                page.release();
            }
            page.page().url.clone()
        };
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ADDRESS).default_value(url));
        self.open_field(tile, Field::Address, input, window, cx);
    }

    /// ↩ in the address field: `text` is where `tile`'s page goes. A new address is the
    /// item's, for every client to follow (`reconcile_browsers` takes this one there too);
    /// the one it has loads again.
    pub(super) fn load_address(&mut self, tile: TileRef, text: &str, cx: &mut Context<Self>) {
        let Some(url) = crate::browser::web_url(text) else { return };
        let Some(ItemKind::Browser { url: at }) = self.item(tile).map(|i| &i.kind) else { return };
        if *at == url {
            if let Some(view) = self.browsers.get(&tile.item) {
                view.update(cx, |v, cx| v.go_to(&url, cx));
            }
        } else {
            tracing::info!(item = %tile.item, %url, "a page is sent to a new address");
            self.propose(tile.worker, ItemOp::SetUrl { id: tile.item, url }, cx);
        }
    }

    /// The focused tile's page, when the focused tile is one.
    fn focused_page(&self) -> Option<&Entity<BrowserView>> {
        self.browsers.get(&self.focused()?.item)
    }

    /// Whether the page's own keys (⌘← and ⌘→) are the workspace's to take: a page is focused
    /// and does not hold the keyboard, which would want them for its fields.
    pub(super) fn page_keys(&self, cx: &App) -> bool {
        self.focused_page().is_some_and(|v| !v.read(cx).focused())
    }

    /// The focused page back one page.
    pub fn page_back(&mut self, _: &PageBack, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.focused_page().cloned() {
            view.update(cx, BrowserView::back);
        }
    }

    /// The focused page forward one page.
    pub fn page_forward(&mut self, _: &PageForward, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.focused_page().cloned() {
            view.update(cx, BrowserView::forward);
        }
    }

    /// Load the focused page again.
    pub fn reload_page(&mut self, _: &ReloadPage, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.focused_page().cloned() {
            view.update(cx, BrowserView::reload);
        }
    }

    /// A pop-up or a `_blank` link of `item`'s page: a new page tile right of it, on its
    /// worker, as "Open URL…" makes one (an open tile for the address is focused instead).
    fn open_beside(&mut self, item: ItemId, url: &str, cx: &mut Context<Self>) {
        let Some(tile) = self.tile_of(item) else { return };
        if self.focused() != Some(tile) {
            self.focus_tile(tile, cx);
        }
        self.open_browser(Some(tile.worker), url, cx);
    }

    /// `item`'s page closed its own window: the tile closes as ⌘W would close it, so "Undo
    /// close" brings it back.
    fn close_page(&mut self, item: ItemId, cx: &mut Context<Self>) {
        let Some(tile) = self.tile_of(item) else { return };
        let Some(page) = self.item(tile).cloned() else { return };
        tracing::info!(%item, "a page closed its window: its tile goes");
        self.remember_closed(tile, page, None, cx);
    }

    /// "Inspect page": Web Inspector on the focused page (a debug build of the Mac app).
    pub fn inspect_page(&mut self, _: &InspectPage, _window: &mut Window, cx: &mut Context<Self>) {
        let opened = self.focused_page().is_some_and(|v| v.read(cx).inspect());
        if !opened {
            self.show_notice("Web Inspector needs a page in front, in a debug build".into(), cx);
        }
    }

    /// ⌘F on a focused page: its find bar. Whether a page took it.
    pub fn find_in_page(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.focused_page().cloned() else { return false };
        view.update(cx, |view, cx| view.find(window, cx));
        true
    }

    /// ⌘+, ⌘− or ⌘0 on a focused page: its zoom, not the terminals' text. Whether a page
    /// took it.
    pub fn zoom_page(&self, step: Zoom, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.focused_page().cloned() else { return false };
        view.update(cx, |view, cx| view.zoom_by(step, cx));
        true
    }

    /// The app draws a dialog over the workspace (settings, adding a worker): pages hide.
    pub fn set_covered(&mut self, covered: bool, cx: &mut Context<Self>) {
        if self.covered != covered {
            self.covered = covered;
            cx.notify();
        }
    }

    /// The view of a browser item, once it has one.
    #[must_use]
    pub fn browser(&self, id: ItemId) -> Option<&Entity<BrowserView>> {
        self.browsers.get(&id)
    }

    /// A view for every browser item; the ones whose items are gone go (and their pages with
    /// them).
    pub(super) fn reconcile_browsers(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<(ItemId, WorkerKey, String)> = self
            .items()
            .filter_map(|(key, item)| match &item.kind {
                ItemKind::Browser { url } => Some((item.id, key, url.clone())),
                _ => None,
            })
            .collect();
        for (id, key, url) in &wanted {
            if let Some(view) = self.browsers.get(id) {
                // The item is the one address every client shares: a new one, typed here or
                // on another client, is where the page goes.
                if view.read(cx).url() != url {
                    view.update(cx, |v, cx| v.go_to(url, cx));
                }
                continue;
            }
            let theme = self.theme.clone();
            let view = cx.new(|_cx| BrowserView::new(*id, *key, url, theme));
            let item = *id;
            cx.subscribe(&view, move |this, _view, event, cx| {
                match event {
                    BrowserEvent::Focused => {
                        // A click in the page ends an edit of its address, as one anywhere
                        // else does.
                        if this.rename.as_ref().is_some_and(|r| r.tile.item == item) {
                            this.finish_rename(false, false, cx);
                        }
                        if let Some(tile) = this.tile_of(item)
                            && this.focused() != Some(tile)
                        {
                            this.focus_tile(tile, cx);
                        }
                    }
                    BrowserEvent::Released => this.pending_focus_self = true,
                    BrowserEvent::Open(url) => this.open_beside(item, url, cx),
                    BrowserEvent::Closed => this.close_page(item, cx),
                }
                cx.notify();
            })
            .detach();
            // The header shows the page's title and address, and it is the workspace's.
            cx.observe(&view, |_this, _view, cx| cx.notify()).detach();
            self.browsers.insert(*id, view);
        }
        self.browsers.retain(|id, _| wanted.iter().any(|(w, ..)| w == id));
        let browsers = &self.browsers;
        self.browser_links.retain(|id, _| browsers.contains_key(id));
    }

    /// Where each page loads on this client: an address on the worker's loopback goes to the
    /// local port this client serves that port on, asked of the worker's link the first time
    /// and again whenever the link is a new one.
    pub(super) fn serve_browsers(&mut self, cx: &mut Context<Self>) {
        let views: Vec<Entity<BrowserView>> = self.browsers.values().cloned().collect();
        for view in views {
            let (id, key, url, needs) = {
                let v = view.read(cx);
                (v.id(), v.worker(), v.url().to_owned(), v.needs_local())
            };
            let Some(port) = crate::browser::worker_port(&url) else {
                if needs {
                    view.update(cx, |v, cx| v.set_local(Some(url), cx));
                }
                continue;
            };
            let Some(remote) = self.workers.get(&key).and_then(|w| w.link.as_ref()?.remote.clone())
            else {
                continue;
            };
            let same_link = self
                .browser_links
                .get(&id)
                .is_some_and(|w| std::sync::Weak::ptr_eq(w, &Arc::downgrade(&remote)));
            if same_link && !needs {
                continue;
            }
            self.browser_links.insert(id, Arc::downgrade(&remote));
            let local = remote.forward(port);
            tracing::info!(item = %id, port, ?local, "browser tile's port served here");
            view.update(cx, |v, cx| match local {
                Some(local) => v.set_local(Some(crate::browser::local_url(&url, local)), cx),
                None => v.unreachable(format!("Port {port} could not be served here"), cx),
            });
        }
    }

    /// What GPUI draws over the strip this frame, for the pages to hide under.
    fn cover(&self) -> Cover {
        let frame = self.drawn.builds.get();
        Cover {
            overlay: self.covered
                || self.palette.is_some()
                || self.search_shown()
                || self.picker.is_some()
                || self.menu.is_some()
                || self.nav.open,
            overview: self.layout.overview_open() || self.drawn.zoom.get() < 1.0,
            toast: self.toast_drawn.get().filter(|(drawn, _)| *drawn == frame).map(|(_, b)| Rect {
                x: f32::from(b.origin.x),
                y: f32::from(b.origin.y),
                w: f32::from(b.size.width),
                h: f32::from(b.size.height),
            }),
        }
    }

    /// Where each page goes, over its tile's body where the strip drew it in this build, or
    /// hidden.
    fn browser_placements(
        &self,
        cx: &App,
    ) -> Vec<(Entity<BrowserView>, crate::browser::Placement)> {
        let drawn = self.drawn.browsers.borrow();
        let cover = self.cover();
        let v = self.drawn.viewport.get();
        let strip = Rect {
            x: f32::from(v.origin.x),
            y: f32::from(v.origin.y),
            w: f32::from(v.size.width),
            h: f32::from(v.size.height),
        };
        self.browsers
            .iter()
            .map(|(id, view)| {
                let alpha = drawn.iter().find(|(drawn, _)| drawn == id).map(|(_, alpha)| *alpha);
                let body = alpha.and_then(|_| view.read(cx).page_area()).map(|b| Rect {
                    x: f32::from(b.origin.x),
                    y: f32::from(b.origin.y),
                    w: f32::from(b.size.width),
                    h: f32::from(b.size.height),
                });
                let alpha = alpha.unwrap_or_default();
                (view.clone(), crate::browser::placement(body, strip, alpha, cover))
            })
            .collect()
    }

    /// An empty element, the strip's last, whose prepaint, after every tile's and the notices',
    /// puts the pages where the tiles were drawn. The workspace is read; the pages are told.
    pub(super) fn browser_sync(cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let this = cx.weak_entity();
        canvas(
            move |_bounds, window, cx| {
                let Some(this) = this.upgrade() else { return };
                let placements = this.read(cx).browser_placements(cx);
                for (view, placement) in placements {
                    if !view.read(cx).place(placement) {
                        view.update(cx, |v, cx| v.open_at(placement, window, cx));
                    }
                }
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .size_0()
        .into_any_element()
    }
}
