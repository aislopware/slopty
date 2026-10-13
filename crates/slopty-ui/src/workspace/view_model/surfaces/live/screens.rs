//! A chat's machine's remote windows and desktop, as right-pane surfaces: the shell asks what
//! the machine can show (`slopty.ListScreens`), shows one (`slopty.OpenScreen`), which puts a
//! window or display item on the machine whose stream the workspace opens as for any, and lets
//! it go (`slopty.CloseScreen`). A screen's id is `screen:` and its item's.

use std::collections::HashMap;

use gpui::{App, Context, Focusable as _, SharedString, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_core::{DisplayId, ItemId, WindowId};
use slopty_proto::ClientMsg;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::screen::{CaptureTarget, DisplayInfo, ScreenFailure, ScreenRequest, WindowInfo};
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::oneshot;

use super::{Live, LiveView, later, now, param};
use crate::workspace::WorkspaceView;

/// What a screen's id starts with.
pub(super) const PREFIX: &str = "screen:";

/// Where a listing's answer goes.
type Reply = oneshot::Sender<Result<RpcReply, RpcError>>;

/// The listings asked for and not come.
#[derive(Default)]
pub(super) struct Screens {
    /// By machine, the answers waiting on its next listing.
    listing: HashMap<WorkerKey, Vec<Reply>>,
}

/// The item a screen's id names.
fn item_of(id: &str) -> Option<ItemId> {
    id.strip_prefix(PREFIX)?.parse().ok()
}

/// A screen's id.
fn screen_id(item: ItemId) -> String {
    format!("{PREFIX}{item}")
}

/// What a listing's choice names: `window:N` or `display:N`.
fn target_of(choice: &str) -> Option<CaptureTarget> {
    let (kind, n) = choice.split_once(':')?;
    let n: u32 = n.parse().ok()?;
    match kind {
        "window" => Some(CaptureTarget::Window(WindowId(n))),
        "display" => Some(CaptureTarget::Display(DisplayId(n))),
        _ => None,
    }
}

impl WorkspaceView {
    /// `slopty.ListScreens`: the windows and displays of `params.chatId`'s machine.
    pub(super) fn live_list_screens(
        &self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some((key, _)) = self.live_place(params, cx) else {
            return now(Err(RpcError::Failed("no machine to show".to_owned())));
        };
        if !self.reachable_for(key, "the window list", cx) {
            let name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
            return now(Err(RpcError::Failed(format!("{name} is out of reach"))));
        }
        let (reply, answer) = oneshot::channel();
        let waiting = cx.global_mut::<Live>().screens.listing.entry(key).or_default();
        let asked = !waiting.is_empty();
        waiting.push(reply);
        if !asked {
            self.send(key, ClientMsg::Screen(ScreenRequest::List));
        }
        later(answer, cx)
    }

    /// `key` listed its windows and displays: the shell's listings asked of it are answered,
    /// its displays first, as a desktop each.
    #[expect(
        clippy::unused_self,
        reason = "the workspace's, as its other answers to a listing are"
    )]
    pub fn live_listing(
        &self,
        key: WorkerKey,
        windows: &[WindowInfo],
        displays: &[DisplayInfo],
        cx: &mut Context<Self>,
    ) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if !live.screens.listing.contains_key(&key) {
            return;
        }
        let replies = cx.global_mut::<Live>().screens.listing.remove(&key).unwrap_or_default();
        let several = displays.len() > 1;
        let desktops = displays.iter().enumerate().map(|(n, display)| {
            let title = if several {
                format!("Desktop {}", n.saturating_add(1))
            } else {
                "Desktop".to_owned()
            };
            serde_json::json!({
                "id": format!("display:{}", display.id.0),
                "title": title,
                "app": "",
                "kind": "display",
            })
        });
        let windows = windows.iter().map(|window| {
            serde_json::json!({
                "id": format!("window:{}", window.id.0),
                "title": window.title,
                "app": window.app,
                "kind": "window",
            })
        });
        let list = serde_json::Value::Array(desktops.chain(windows).collect());
        for reply in replies {
            let _gone = reply.send(Ok(RpcReply::Value(list.clone())));
        }
    }

    /// `key` could not list its windows: the shell's listings asked of it are told why.
    pub fn live_listing_failed(&self, key: WorkerKey, why: &ScreenFailure, cx: &mut App) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if !live.screens.listing.contains_key(&key) {
            return;
        }
        let name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
        let text = crate::screen::failure_text(why, &name);
        let replies = cx.global_mut::<Live>().screens.listing.remove(&key).unwrap_or_default();
        for reply in replies {
            let _gone = reply.send(Err(RpcError::Failed(text.clone())));
        }
    }

    /// `slopty.OpenScreen`: show `params.screen` (a listing's id) of `params.chatId`'s
    /// machine, titled `params.title`. One already shown is shown again.
    pub(super) fn live_open_screen(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let (key, _) = self
            .live_place(params, cx)
            .ok_or_else(|| RpcError::Failed("no machine to show".to_owned()))?;
        let target = param(params, "screen")
            .and_then(target_of)
            .ok_or_else(|| RpcError::BadParams("screen is required".to_owned()))?;
        let kind = match target {
            CaptureTarget::Window(window) => ItemKind::Window { window },
            CaptureTarget::Display(display) => ItemKind::Display { display },
        };
        let w = self.workers.get(&key).ok_or_else(|| RpcError::Failed("no machine".to_owned()))?;
        if let Some(item) = w.doc.items().find(|item| item.kind == kind) {
            return Ok(serde_json::json!({ "id": screen_id(item.id) }));
        }
        let item =
            Item { id: ItemId::new(), kind, name: None, facts: std::collections::BTreeMap::new() };
        let id = item.id;
        let title = param(params, "title").unwrap_or("Remote window").to_owned();
        self.titles.insert(id, title);
        self.propose(key, ItemOp::Add(item), cx);
        Ok(serde_json::json!({ "id": screen_id(id) }))
    }

    /// `slopty.CloseScreen`: the screen `params.id` is let go, and its stream with it.
    pub(super) fn live_close_screen(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let item = param(params, "id")
            .and_then(item_of)
            .ok_or_else(|| RpcError::BadParams("id is required".to_owned()))?;
        if let Some(tile) = self.tile_of(item) {
            self.propose(tile.worker, ItemOp::Remove(item), cx);
        }
        Ok(serde_json::json!({}))
    }

    /// The screen `id`'s view, once its stream has one.
    pub(super) fn live_screen_view(&self, id: &str, cx: &App) -> Option<LiveView> {
        let item = item_of(id)?;
        let view = self.screens.get(&item)?.clone();
        let focus = view.read(cx).focus_handle(cx);
        let title = self.titles.get(&item).map(|title| SharedString::from(title.clone()));
        Some(LiveView { view: view.into(), focus, title, ended: false })
    }

    /// Call `changed` as the workspace changes, so a screen waiting for its stream shows it
    /// once it has one.
    #[expect(clippy::unused_self, reason = "the workspace's, as the terminal's watch is")]
    pub(super) fn live_watch_screen(
        &self,
        id: &str,
        mut changed: Box<dyn FnMut(&mut App)>,
        cx: &mut Context<Self>,
    ) -> Option<Subscription> {
        item_of(id)?;
        let this = cx.entity();
        let app: &mut App = cx;
        Some(app.observe(&this, move |_, cx| changed(cx)))
    }
}
