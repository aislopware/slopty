//! Each worker's items kept on this device ([`ItemCache`]): read when the worker is added, so
//! a cold launch draws its tiles as they were under the pill saying where it is (reconnecting,
//! unreachable, on another build with its Update) rather than holes; written after every change
//! to its registry, off the UI thread, one write per worker at a time and the latest last.
//!
//! A tile the cache does not have (none was kept yet, or this build could not read what an
//! older one kept) is still drawn while its worker is away: the worker's name over the same
//! pill, so no workspace opens on a gap.

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, ElementId, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_client::items::{ItemCache, ItemDoc};
use slopty_client::layout::{Placed, WorkerKey};

use super::WorkspaceView;
use super::tile::{Chrome, SHAPES_BELOW, title_ink};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::IconName;

/// The cache and the writes under way.
#[derive(Default)]
pub(super) struct KeptItems {
    cache: Option<ItemCache>,
    /// Workers whose items are being written now.
    writing: HashSet<WorkerKey>,
    /// Workers whose items changed while being written: written again once that is done.
    again: HashSet<WorkerKey>,
}

impl WorkspaceView {
    /// Keep each worker's items under `dir`, and draw a worker's tiles from them before it is
    /// linked. Set before the workers are added.
    pub fn set_item_cache(&mut self, dir: PathBuf) {
        self.kept_items.cache = Some(ItemCache::new(dir));
    }

    /// `key`'s items as last kept, when there are any.
    pub(super) fn kept_doc(&self, key: WorkerKey) -> Option<ItemDoc> {
        let items = self.kept_items.cache.as_ref()?.items(&key.to_string());
        (!items.is_empty()).then(|| ItemDoc::cached(items))
    }

    /// Write `key`'s items as they are now, after the write under way when there is one.
    pub(super) fn keep_items(&mut self, key: WorkerKey, cx: &Context<Self>) {
        let Some(cache) = self.kept_items.cache.clone() else { return };
        if !self.kept_items.writing.insert(key) {
            self.kept_items.again.insert(key);
            return;
        }
        let Some(w) = self.workers.get(&key) else {
            self.kept_items.writing.remove(&key);
            return;
        };
        let bytes = ItemCache::encode(w.doc.items());
        let name = key.to_string();
        cx.spawn(async move |this, cx| {
            let write = async move { bytes.and_then(|bytes| cache.write(&name, &bytes)) };
            let written = cx.background_executor().spawn(write).await;
            if let Err(error) = written {
                tracing::debug!(%error, "items not kept");
            }
            let _gone = this.update(cx, |this, cx| {
                this.kept_items.writing.remove(&key);
                if this.kept_items.again.remove(&key) {
                    this.keep_items(key, cx);
                }
            });
        })
        .detach();
    }
}

impl WorkspaceView {
    /// A tile whose item is not here while its worker is away: the worker's name in the header
    /// and the pill its tiles show ([`Self::away_state`]), with its Retry, Wake or Update.
    /// `None` while the worker is linked: its registry is the truth then, and a tile it lacks
    /// leaves with its next snapshot.
    pub(super) fn render_missing(
        &self,
        placed: &Placed,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let tile = placed.tile;
        let state = self.away_state(tile.worker)?;
        let theme = &self.theme;
        let k = chrome.k;
        let id = tile.item;
        let name = SharedString::from(self.worker_name(tile.worker));
        let shapes = k < SHAPES_BELOW;
        let ink = hsla(title_ink(theme, placed.focused));
        let header = div()
            .id("title")
            .debug_selector(move || format!("title-{}", id.as_uuid()))
            .role(Role::Heading)
            .aria_label(name.clone())
            .h(px(theme.density.header * k))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .px(px(theme.spacing.inset() * k))
            .overflow_hidden()
            .whitespace_nowrap()
            .bg(hsla(theme.content()))
            .text_size(px(theme.typography.ui_size * k))
            .text_color(ink)
            .font_family(theme.typography.ui_family.clone())
            .when(!shapes, |el| {
                el.child(crate::palette::status_slot(theme, IconName::Server, None, ink, k))
                    .child(name.clone())
            });
        let pill = (!shapes).then(|| self.render_state_pill(tile, &state, true, chrome, cx));
        let rect = placed.rect;
        let (width, height) = (rect.w * placed.scale, rect.h * placed.scale);
        let (left, top) = (rect.x + (rect.w - width) / 2.0, rect.y + (rect.h - height) / 2.0);
        Some(
            div()
                .id(ElementId::Uuid(*id.as_uuid()))
                .debug_selector(move || format!("item-{}", id.as_uuid()))
                .role(Role::Group)
                .aria_label(name)
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(height))
                .opacity(placed.alpha)
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(hsla(theme.content()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| this.click_tile(tile, cx)),
                )
                .child(header)
                .child(
                    div()
                        .debug_selector(move || format!("missing-{}", id.as_uuid()))
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .relative()
                        .children(pill),
                )
                .into_any_element(),
        )
    }
}
