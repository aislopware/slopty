//! The browser's previews: the ports the chat's machine listens on, as its shells reported them,
//! each forwarded to a port of this client's loopback (`slopty_client::tunnel`). The browser
//! loads a preview at its local address, so the page is the worker's as a local one would be.

use std::collections::HashMap;

use futures::StreamExt as _;
use gpui::{Context, Task};
use slopty_client::WorkerKey;
use slopty_view_proto::{PreviewService, PreviewSnapshot};
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::watch;

use super::{Live, now, param};
use crate::workspace::WorkspaceView;

/// The previews watched, by the chat they are for.
#[derive(Default)]
pub(super) struct Previews {
    /// Each watched chat's machine, and its previews as last published.
    watched: HashMap<String, (WorkerKey, watch::Sender<PreviewSnapshot>)>,
}

impl WorkspaceView {
    /// `WatchPreviews`: the forwarded ports of `params.chatId`'s machine, now and each time
    /// they change.
    pub(super) fn live_watch_previews(
        &self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(chat) = param(params, "chatId").map(str::to_owned) else {
            return now(Err(RpcError::BadParams("chatId is required".to_owned())));
        };
        let Some((key, _)) = self.live_place(params, cx) else {
            return now(Err(RpcError::Failed("no machine to preview".to_owned())));
        };
        let snapshot = self.live_previews(key);
        let live = cx.global_mut::<Live>();
        let (_, sender) = live
            .previews
            .watched
            .entry(chat)
            .or_insert_with(|| (key, watch::Sender::new(snapshot)));
        let frames =
            futures::stream::unfold((sender.subscribe(), true), async |(mut rx, first)| {
                if !first && rx.changed().await.is_err() {
                    return None;
                }
                let value = serde_json::to_value(&*rx.borrow_and_update()).ok()?;
                Some((value, (rx, false)))
            });
        Task::ready(Ok(RpcReply::Stream(frames.boxed())))
    }

    /// Publish the previews of every watched chat that changed; let go of those no one
    /// watches any more.
    pub(super) fn live_publish_previews(&self, cx: &mut Context<Self>) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if live.previews.watched.is_empty() {
            return;
        }
        let fresh: Vec<(String, PreviewSnapshot)> = live
            .previews
            .watched
            .iter()
            .map(|(chat, (key, _))| (chat.clone(), self.live_previews(*key)))
            .collect();
        let watched = &mut cx.global_mut::<Live>().previews.watched;
        for (chat, snapshot) in fresh {
            if let Some((_, sender)) = watched.get(&chat) {
                sender.send_if_modified(|now| {
                    let changed = *now != snapshot;
                    *now = snapshot;
                    changed
                });
            }
        }
        watched.retain(|_, (_, sender)| sender.receiver_count() > 0);
    }

    /// The previews of `key`: every port its shells listen on that is forwarded here.
    fn live_previews(&self, key: WorkerKey) -> PreviewSnapshot {
        let device = key.to_string();
        let device_name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
        let mut services: Vec<PreviewService> = self
            .ports
            .iter()
            .filter(|(session, _)| self.worker_of_session(**session) == Some(key))
            .flat_map(|(_, forwards)| forwards)
            .filter_map(|forward| {
                let local = forward.local?;
                Some(PreviewService {
                    id: format!("{device}:{}", forward.port.number),
                    device_name: device_name.clone(),
                    // The address here: the browser loads the forward, not the worker's port.
                    hostname: format!("localhost:{local}"),
                    name: forward.port.process.clone(),
                    port: forward.port.number,
                })
            })
            .collect();
        services.sort_by_key(|service| service.port);
        services.dedup_by_key(|service| service.port);
        PreviewSnapshot { services, remote: true }
    }
}
