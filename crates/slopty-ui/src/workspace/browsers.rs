//! Web pages in tiles: opening one (a forwarded port, an address typed into the palette), a
//! view per browser item, and each frame the native page put over its tile or hidden.

use gpui::{AppContext as _, Context, Entity, IntoElement as _, Styled as _, Window, canvas};
use slopty_client::layout::{Rect, WorkerKey};
use slopty_core::ItemId;
use slopty_proto::items::{Item, ItemKind, ItemOp};

use super::WorkspaceView;
use super::actions::OpenUrl;
use crate::browser::{BrowserEvent, BrowserView, Cover};
use crate::palette::CommandPalette;

/// What the "Open URL…" palette starts with: the forwarded ports live on localhost.
const URL_SEED: &str = "http://localhost:";

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
        self.propose(key, ItemOp::Upsert(item), cx);
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
        let wanted: Vec<(ItemId, String)> = self
            .items()
            .filter_map(|(_, item)| match &item.kind {
                ItemKind::Browser { url } => Some((item.id, url.clone())),
                _ => None,
            })
            .collect();
        for (id, url) in &wanted {
            if self.browsers.contains_key(id) {
                continue;
            }
            let theme = self.theme.clone();
            let view = cx.new(|_cx| BrowserView::new(*id, url, theme));
            let item = *id;
            self.subscriptions.push(cx.subscribe(&view, move |this, _view, event, cx| {
                match event {
                    BrowserEvent::Focused => {
                        if let Some(tile) = this.tile_of(item)
                            && this.focused() != Some(tile)
                        {
                            this.focus_tile(tile, cx);
                        }
                    }
                    BrowserEvent::Released => this.pending_focus_self = true,
                }
                cx.notify();
            }));
            // The header shows the page's title and address, and it is the workspace's.
            self.subscriptions.push(cx.observe(&view, |_this, _view, cx| cx.notify()));
            self.browsers.insert(*id, view);
        }
        self.browsers.retain(|id, _| wanted.iter().any(|(w, _)| w == id));
    }

    /// What GPUI draws over the strip this frame, for the pages to hide under.
    fn cover(&self) -> Cover {
        Cover {
            overlay: self.covered
                || self.palette.is_some()
                || self.picker.is_some()
                || self.menu.is_some(),
            overview: self.layout.overview_open() || self.drawn_zoom < 1.0,
        }
    }

    /// Each page over its tile's body as drawn this frame, or hidden.
    fn sync_browsers(&self, window: &Window, cx: &mut Context<Self>) {
        let frame = self.frames_drawn;
        let cover = self.cover();
        let v = self.viewport;
        let strip = Rect {
            x: f32::from(v.origin.x),
            y: f32::from(v.origin.y),
            w: f32::from(v.size.width),
            h: f32::from(v.size.height),
        };
        for view in self.browsers.values() {
            let (body, alpha) = {
                let v = view.read(cx);
                let body = v.drawn_in(frame).map(|b| Rect {
                    x: f32::from(b.origin.x),
                    y: f32::from(b.origin.y),
                    w: f32::from(b.size.width),
                    h: f32::from(b.size.height),
                });
                (body, v.alpha())
            };
            let placement = crate::browser::placement(body, strip, alpha, cover);
            view.update(cx, |v, cx| v.apply(placement, window, cx));
        }
    }

    /// An empty element whose prepaint, after every tile's, puts the pages where the tiles
    /// were drawn.
    pub(super) fn browser_sync(cx: &Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity();
        canvas(
            move |_bounds, window, cx| entity.update(cx, |this, cx| this.sync_browsers(window, cx)),
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .size_0()
        .into_any_element()
    }
}
