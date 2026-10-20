//! The shell's live surfaces, answered from the workspace: a chat's terminals, its machine's
//! remote windows and desktop, and the ports its machine serves, forwarded here for the browser.
//!
//! The shell draws Slopty's own views of these (`slopty_shell::live`): its calls here name them
//! and the app hands the shell each view by the id answered. A terminal's id is its session's,
//! a screen's the id of its window or display item. Every shell opened for the interface carries
//! the fact `chat` (the chat it was opened for), so the shells still running after the app comes
//! back are found again by the chat that asks.

mod previews;
mod screens;
mod terminals;

use gpui::{App, AppContext as _, Context, Global, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_view_rpc::{RpcError, RpcReply, methods};

use super::super::param;
use crate::view_model::live::{LiveLook, LiveView};
use crate::workspace::WorkspaceView;

/// What the shell asks of the live surfaces beyond zeron's own calls.
pub mod calls {
    /// The windows and displays of the chat's machine: `{ chatId }` → `[{ id, title, app,
    /// display }]`.
    pub const LIST_SCREENS: &str = "slopty.ListScreens";
    /// Show a window or a display of the chat's machine: `{ chatId, window | display, title }`
    /// → `{ id }`, the screen's id.
    pub const OPEN_SCREEN: &str = "slopty.OpenScreen";
    /// Stop showing a screen: `{ id }`.
    pub const CLOSE_SCREEN: &str = "slopty.CloseScreen";
}

/// The live surfaces' own state, one per app.
#[derive(Default)]
struct Live {
    terminals: terminals::Terminals,
    screens: screens::Screens,
    previews: previews::Previews,
    /// Hears the workspace change, to settle what waits on it.
    hearing: Option<Subscription>,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live").finish_non_exhaustive()
    }
}

impl Global for Live {}

/// The answer of a call made now.
fn now(answer: Result<serde_json::Value, RpcError>) -> Task<Result<RpcReply, RpcError>> {
    Task::ready(answer.map(RpcReply::Value))
}

/// The answer of a call that waits on the workspace, or that the workspace has gone.
fn later(
    answer: tokio::sync::oneshot::Receiver<Result<RpcReply, RpcError>>,
    cx: &App,
) -> Task<Result<RpcReply, RpcError>> {
    cx.background_spawn(async move { answer.await.unwrap_or(Err(RpcError::Closed)) })
}

impl WorkspaceView {
    /// The answer to `method` when a live surface makes it; `None` for every other call.
    pub(super) fn live_surface_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        Self::live_hear(cx);
        Some(match method {
            methods::OPEN_TERMINAL => self.live_open_terminal(params, cx),
            methods::CLOSE_TERMINAL => now(self.live_close_terminal(params, cx)),
            methods::WATCH_PREVIEWS => self.live_watch_previews(params, cx),
            calls::LIST_SCREENS => self.live_list_screens(params, cx),
            calls::OPEN_SCREEN => now(self.live_open_screen(params, cx)),
            calls::CLOSE_SCREEN => now(self.live_close_screen(params, cx)),
            _ => return None,
        })
    }

    /// Hear the workspace change from the first call on, to settle what waits on it.
    fn live_hear(cx: &mut Context<Self>) {
        if cx.default_global::<Live>().hearing.is_some() {
            return;
        }
        let this = cx.entity();
        let hearing = cx.observe(&this, |view, _, cx| view.live_settle(cx));
        cx.global_mut::<Live>().hearing = Some(hearing);
    }

    /// Answer what waited on the workspace and is there now.
    fn live_settle(&mut self, cx: &mut Context<Self>) {
        self.live_settle_again(cx);
        self.live_settle_opens(cx);
        self.live_settle_terminals(cx);
        self.live_publish_previews(cx);
    }

    /// The machine and folder a chat of the interface stands in ([`Self::live_chat_place`]),
    /// else the machine in context.
    fn live_place(
        &self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<(WorkerKey, Option<String>)> {
        self.live_chat_place(params, cx)
            .or_else(|| self.context_worker().map(|worker| (worker, None)))
    }

    /// The machine and folder the chat `params.chatId` itself names: its thread's, one made in
    /// the interface and not started, or a project's new-session canvas (`space-canvas:{space}`).
    /// `params.targetDeviceId`, when given, names the machine. `None` while the workspace knows
    /// no such chat or machine (yet).
    fn live_chat_place(
        &self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<(WorkerKey, Option<String>)> {
        let chat = param(params, "chatId").unwrap_or_default();
        let target = param(params, "targetDeviceId").and_then(|device| self.view_worker(device));
        if let Some(space) = chat.strip_prefix("space-canvas:")
            && let Some((worker, path)) = self.view_space(space)
        {
            return Some((target.unwrap_or(worker), Some(path)));
        }
        if let Some(minted) = self.view_model.minted.get(chat) {
            return Some((target.unwrap_or(minted.worker), Some(minted.cwd.clone())));
        }
        if let Some((worker, thread)) = self.chat_thread(chat, cx) {
            let cwd = self.thread_place(thread).and_then(|place| place.cwd.clone());
            return Some((target.unwrap_or(worker), cwd));
        }
        target.map(|worker| (worker, None))
    }

    /// The terminal `id`, in the frame the shell's `look` says; `None` until it is attached.
    pub fn hosted_terminal(
        &self,
        id: &str,
        look: LiveLook,
        cx: &mut Context<Self>,
    ) -> Option<LiveView> {
        self.live_terminal_view(id, look, cx)
    }

    /// The screen `id`; `None` while it has no view.
    pub fn hosted_screen(&self, id: &str, cx: &App) -> Option<LiveView> {
        self.live_screen_view(id, cx)
    }

    /// Call `changed` when the view `id` says something new of itself: a terminal's title or
    /// end, a screen's title.
    pub fn hosted_watch(
        &self,
        id: &str,
        changed: Box<dyn FnMut(&mut App)>,
        cx: &mut Context<Self>,
    ) -> Option<Subscription> {
        if id.starts_with(screens::PREFIX) {
            return self.live_watch_screen(id, changed, cx);
        }
        self.live_watch_terminal(id, changed, cx)
    }
}
