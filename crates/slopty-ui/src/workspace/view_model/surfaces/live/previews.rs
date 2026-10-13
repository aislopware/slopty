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
use crate::view_model::rows;
use crate::workspace::WorkspaceView;

/// The previews watched, by the chat they are for.
#[derive(Default)]
pub(super) struct Previews {
    /// Each watched chat's machine and folder, and its previews as last published.
    watched: HashMap<String, (WorkerKey, Option<String>, watch::Sender<PreviewSnapshot>)>,
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
        let Some((key, folder)) = self.live_place(params, cx) else {
            return now(Err(RpcError::Failed("no machine to preview".to_owned())));
        };
        let snapshot = self.live_previews(key, folder.as_deref());
        let live = cx.global_mut::<Live>();
        let (_, _, sender) = live
            .previews
            .watched
            .entry(chat)
            .or_insert_with(|| (key, folder, watch::Sender::new(snapshot)));
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
            .map(|(chat, (key, folder, _))| {
                (chat.clone(), self.live_previews(*key, folder.as_deref()))
            })
            .collect();
        let watched = &mut cx.global_mut::<Live>().previews.watched;
        for (chat, snapshot) in fresh {
            if let Some((_, _, sender)) = watched.get(&chat) {
                sender.send_if_modified(|now| {
                    let changed = *now != snapshot;
                    *now = snapshot;
                    changed
                });
            }
        }
        watched.retain(|_, (_, _, sender)| sender.receiver_count() > 0);
    }

    /// The previews of `key`: every port its shells listen on that is forwarded here.
    fn live_previews(&self, key: WorkerKey, folder: Option<&str>) -> PreviewSnapshot {
        let device = key.to_string();
        let device_name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
        let project_cwd = folder.unwrap_or_default().to_owned();
        let project_name =
            project_cwd.rsplit('/').find(|part| !part.is_empty()).unwrap_or_default().to_owned();
        let project_id = rows::space_id(&device, &project_cwd);
        let mut services: Vec<PreviewService> = self
            .ports
            .iter()
            .filter(|(session, _)| self.worker_of_session(**session) == Some(key))
            .flat_map(|(session, forwards)| forwards.iter().map(move |forward| (*session, forward)))
            .filter_map(|(session, forward)| {
                let local = forward.local?;
                Some(PreviewService {
                    id: format!("{device}:{}", forward.port.number),
                    project_id: project_id.clone(),
                    project_name: project_name.clone(),
                    project_cwd: project_cwd.clone(),
                    device_id: device.clone(),
                    device_name: device_name.clone(),
                    // The address here: the browser loads the forward, not the worker's port.
                    hostname: format!("localhost:{local}"),
                    name: forward.port.process.clone(),
                    port: forward.port.number,
                    pid: forward.port.pid,
                    cwd: self.summary(session).and_then(|s| s.cwd.clone()).unwrap_or_default(),
                    started_at: 0,
                    slopty_owned: false,
                })
            })
            .collect();
        services.sort_by_key(|service| service.port);
        services.dedup_by_key(|service| service.port);
        PreviewSnapshot {
            services,
            // Each preview is its own forward: no one proxy serves them.
            proxy_port: 0,
            error: None,
            project_name: (!project_name.is_empty()).then_some(project_name),
            remote: true,
        }
    }
}
