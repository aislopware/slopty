//! A chat's terminals: `OpenTerminal` asks the chat's machine for a shell in its folder, and the
//! answer, once the shell's view is attached, is the session's id; the panel then shows that
//! view. A shell opened for a chat and still running when it is asked again (the app came back,
//! the session lived on in its worker) is found by its `chat` fact and shown again instead of a
//! new one. `CloseTerminal` ends the shell.

use std::collections::{HashMap, HashSet};

use gpui::{App, Context, Focusable as _, SharedString, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_core::SessionId;
use slopty_proto::items::{ItemKind, ItemOp};
use slopty_proto::terminal::{OpenSession, SessionState, TermSize};
use slopty_proto::{ClientMsg, RequestId};
use slopty_terminal::{GridFrame, TerminalViewEvent};
use slopty_view_proto::TerminalSession;
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::oneshot;

use super::{Live, LiveLook, LiveView, later, now, param};
use crate::workspace::WorkspaceView;

/// The fact a shell opened for the interface carries: the chat it was opened for.
const CHAT: &str = crate::workspace::CHAT_FACT;

/// Where an open's answer goes.
type Reply = oneshot::Sender<Result<RpcReply, RpcError>>;

/// The terminals asked for, coming and shown.
#[derive(Default)]
pub(super) struct Terminals {
    /// Asked of a machine, its session not come yet: the chat it is for and the answer.
    asked: HashMap<(WorkerKey, RequestId), (String, Reply)>,
    /// Come (or found again), its view not attached yet.
    coming: HashMap<SessionId, (String, Reply)>,
    /// Shown in a tab of the interface.
    shown: HashSet<SessionId>,
}

/// The program a session runs, as a tab names it: the command's last path part.
fn shell_of(command: &[String]) -> String {
    command.first().and_then(|program| program.rsplit('/').next()).unwrap_or_default().to_owned()
}

impl WorkspaceView {
    /// `OpenTerminal`: a shell for `params.chatId` on its machine, in its folder or
    /// `params.cwd`, sized `params.cols` × `params.rows` until its view fits it.
    pub(super) fn live_open_terminal(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let chat = param(params, "chatId").unwrap_or_default().to_owned();
        let Some((key, cwd)) = self.live_place(params, cx) else {
            return now(Err(RpcError::Failed("no machine to open a terminal on".to_owned())));
        };
        let cwd = param(params, "cwd").map(str::to_owned).or(cwd);
        let (reply, answer) = oneshot::channel();
        if let Some(session) = self.live_kept_shell(key, &chat, cx) {
            cx.global_mut::<Live>().terminals.coming.insert(session, (chat, reply));
            self.live_settle_terminals(cx);
            return later(answer, cx);
        }
        if !self.reachable_for(key, "the terminal", cx) {
            let name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
            return now(Err(RpcError::Failed(format!("{name} is out of reach"))));
        }
        let size = |key: &str, default: u16| {
            params
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u16::try_from(n).ok())
                .unwrap_or(default)
        };
        let spec = OpenSession {
            size: TermSize {
                cols: size("cols", 80),
                rows: size("rows", 24),
                ..TermSize::default()
            },
            cwd,
            command: Vec::new(),
            env: Vec::new(),
            title: None,
            attach: false,
        };
        let request = self.next_open.get();
        self.next_open.set(request.wrapping_add(1));
        cx.global_mut::<Live>().terminals.asked.insert((key, request), (chat, reply));
        tracing::debug!(%key, request, cwd = ?spec.cwd, "a terminal for the interface");
        self.send(key, ClientMsg::OpenSession { request, spec });
        later(answer, cx)
    }

    /// A shell on `key` opened for `chat`, still running and shown nowhere.
    fn live_kept_shell(&self, key: WorkerKey, chat: &str, cx: &App) -> Option<SessionId> {
        let terminals = &cx.try_global::<Live>()?.terminals;
        let w = self.workers.get(&key)?;
        w.doc.items().find_map(|item| {
            let ItemKind::Terminal { session } = item.kind else { return None };
            let alive = w
                .sessions
                .get(&session)
                .is_some_and(|s| !matches!(s.state, SessionState::Exited { .. }));
            let free =
                !terminals.shown.contains(&session) && !terminals.coming.contains_key(&session);
            (alive && free && item.facts.get(CHAT).map(String::as_str) == Some(chat))
                .then_some(session)
        })
    }

    /// The machine `key` opened the shell asked for under `request` as `session`.
    pub fn live_session_opened(
        &mut self,
        key: WorkerKey,
        request: RequestId,
        session: SessionId,
        cx: &mut Context<Self>,
    ) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if !live.terminals.asked.contains_key(&(key, request)) {
            return;
        }
        let terminals = &mut cx.global_mut::<Live>().terminals;
        if let Some(asked) = terminals.asked.remove(&(key, request)) {
            terminals.coming.insert(session, asked);
        }
        self.live_settle_terminals(cx);
    }

    /// The machine `key` could not open the shell asked for under `request`.
    #[expect(clippy::unused_self, reason = "the workspace's, as the open it answers is")]
    pub fn live_open_failed(
        &self,
        key: WorkerKey,
        request: RequestId,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if !live.terminals.asked.contains_key(&(key, request)) {
            return;
        }
        if let Some((_, reply)) = cx.global_mut::<Live>().terminals.asked.remove(&(key, request)) {
            let _gone = reply.send(Err(RpcError::Failed(message.to_owned())));
        }
    }

    /// Answer each open whose shell's view is attached now, and mark its shell as the chat's.
    pub(super) fn live_settle_terminals(&mut self, cx: &mut Context<Self>) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if live.terminals.coming.is_empty() {
            return;
        }
        let coming = std::mem::take(&mut cx.global_mut::<Live>().terminals.coming);
        let mut waiting = HashMap::new();
        for (session, (chat, reply)) in coming {
            if reply.is_closed() {
                continue;
            }
            let Some(key) = self.worker_of_session(session) else {
                waiting.insert(session, (chat, reply));
                continue;
            };
            let item = self.workers.get(&key).and_then(|w| w.doc.item_for_session(session));
            let Some(item) = item.filter(|_| self.terminals.contains_key(&session)) else {
                waiting.insert(session, (chat, reply));
                continue;
            };
            let id = item.id;
            if item.facts.get(CHAT) != Some(&chat) {
                let fact = ItemOp::SetFact { id, key: CHAT.to_owned(), value: Some(chat) };
                self.propose(key, fact, cx);
            }
            let summary = self.summary(session);
            let answer = TerminalSession {
                id: session.to_string(),
                cwd: summary.and_then(|s| s.cwd.clone()).unwrap_or_default(),
                shell: summary.map(|s| shell_of(&s.command)).unwrap_or_default(),
            };
            cx.global_mut::<Live>().terminals.shown.insert(session);
            let _gone = reply.send(RpcReply::value(&answer));
        }
        cx.global_mut::<Live>().terminals.coming.extend(waiting);
    }

    /// `CloseTerminal`: the tab is closed, and its shell ends.
    pub(super) fn live_close_terminal(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let session: SessionId = param(params, "terminalId")
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| RpcError::BadParams("terminalId is required".to_owned()))?;
        cx.global_mut::<Live>().terminals.shown.remove(&session);
        if let Some(key) = self.worker_of_session(session) {
            self.close_session_on(key, session);
            if let Some(tile) = self.tile_of_session(session) {
                self.propose(key, ItemOp::Remove(tile.item), cx);
            }
        }
        Ok(serde_json::json!({}))
    }

    /// The terminal `id`'s view, in the frame `look` says.
    pub(super) fn live_terminal_view(
        &self,
        id: &str,
        look: LiveLook,
        cx: &mut App,
    ) -> Option<LiveView> {
        let session: SessionId = id.parse().ok()?;
        let view = self.terminals.get(&session)?.clone();
        let frame = GridFrame { padding: look.padding, line_height: look.line_height };
        view.update(cx, |view, cx| view.set_frame(Some(frame), cx));
        let read = view.read(cx);
        let title = read.title().map(|title| SharedString::from(title.to_owned()));
        let focus = read.focus_handle(cx);
        let ended =
            self.summary(session).is_none_or(|s| matches!(s.state, SessionState::Exited { .. }));
        Some(LiveView { view: view.into(), focus, title, ended })
    }

    /// Call `changed` when the terminal `id` changes its title or ends.
    pub(super) fn live_watch_terminal(
        &self,
        id: &str,
        mut changed: Box<dyn FnMut(&mut App)>,
        cx: &mut App,
    ) -> Option<Subscription> {
        let session: SessionId = id.parse().ok()?;
        let view = self.terminals.get(&session)?;
        Some(cx.subscribe(view, move |_, event, cx| {
            if matches!(event, TerminalViewEvent::Title(_) | TerminalViewEvent::Exited(_)) {
                changed(cx);
            }
        }))
    }
}
