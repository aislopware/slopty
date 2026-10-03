//! Each worker's items kept on this device ([`ItemCache`]): read when the worker is added, so
//! a cold launch draws its tiles as they were under the pill saying where it is (reconnecting,
//! unreachable, on another build with its Update) rather than holes; written after every change
//! to its registry, off the UI thread, one write per worker at a time and the latest last.

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::Context;
use slopty_client::items::{ItemCache, ItemDoc};
use slopty_client::layout::WorkerKey;

use super::WorkspaceView;

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
