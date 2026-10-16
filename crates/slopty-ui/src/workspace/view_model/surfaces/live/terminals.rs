//! A chat's terminals: `OpenTerminal` asks the chat's machine for a shell in its folder, and the
//! answer, once the shell's view is attached, is the session's id; the panel then shows that
//! view. A shell opened for a chat and still running when it is asked again (the app came back,
//! the session lived on in its worker) is found by its `chat` fact and shown again instead of a
//! new one. A project's run script opens in a terminal of its own. `CloseTerminal` ends the
//! shell. A tab the app showed when it last ran asks for its terminal again by
//! its id (`terminalId`): shown once its machine says it still runs, refused once it ended or
//! [`AGAIN_GRACE`] passed with its machine silent of it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{App, Context, Focusable as _, SharedString, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_core::{ItemId, SessionId};
use slopty_proto::git::RunScript;
use slopty_proto::items::{Item, ItemKind, ItemOp};
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

/// How long a terminal asked for again by its id is waited for: the app's links to its machines,
/// and their sessions, come back after the app does.
pub(super) const AGAIN_GRACE: Duration = Duration::from_secs(20);

/// The terminals asked for, coming and shown.
#[derive(Default)]
pub(super) struct Terminals {
    /// Asked of a machine, its session not come yet: the chat it is for and the answer.
    asked: HashMap<(WorkerKey, RequestId), (String, Reply)>,
    /// Come (or found again), its view not attached yet.
    coming: HashMap<SessionId, (String, Reply)>,
    /// Shown in a tab of the interface.
    shown: HashSet<SessionId>,
    /// Terminals asked for again by their id, by session: the chat, the answer, and until when
    /// its machine is waited for.
    again: HashMap<SessionId, (String, Reply, Instant)>,
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
        if let Some(session) = param(params, "terminalId").and_then(|id| id.parse().ok()) {
            let chat = param(params, "chatId").unwrap_or_default().to_owned();
            return self.live_open_again(session, chat, cx);
        }
        self.live_open_session(params, None, cx)
    }

    /// A terminal for `params.chatId` as [`Self::live_open_terminal`] opens one, running `script`
    /// on `key` where one is named, in a fresh terminal of its own.
    pub(in crate::workspace) fn live_open_session(
        &mut self,
        params: &serde_json::Value,
        script: Option<(WorkerKey, RunScript)>,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let chat = param(params, "chatId").unwrap_or_default().to_owned();
        let place = match &script {
            Some((key, script)) => Some((*key, Some(script.cwd.clone()))),
            None => self.live_place(params, cx),
        };
        let Some((key, cwd)) = place else {
            return now(Err(RpcError::Failed("no machine to open a terminal on".to_owned())));
        };
        let cwd = param(params, "cwd").filter(|_| script.is_none()).map(str::to_owned).or(cwd);
        let (reply, answer) = oneshot::channel();
        if script.is_none()
            && let Some(session) = self.live_kept_shell(key, &chat, cx)
        {
            self.live_hold(session, &chat, cx);
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
            command: script.as_ref().map(|(_, s)| s.command.clone()).unwrap_or_default(),
            env: script.as_ref().map(|(_, s)| s.env.clone()).unwrap_or_default(),
            title: script.map(|(_, s)| s.name),
            attach: false,
        };
        let request = self.next_open.get();
        self.next_open.set(request.wrapping_add(1));
        cx.global_mut::<Live>().terminals.asked.insert((key, request), (chat, reply));
        tracing::debug!(%key, request, cwd = ?spec.cwd, "a terminal for the interface");
        self.send(key, ClientMsg::OpenSession { request, spec });
        later(answer, cx)
    }

    /// Terminal `session` for `chat` again, which a tab showed when the app last ran: answered
    /// once its machine says it still runs, refused once it ended or [`AGAIN_GRACE`] passed.
    fn live_open_again(
        &mut self,
        session: SessionId,
        chat: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (reply, answer) = oneshot::channel();
        let until = Instant::now().checked_add(AGAIN_GRACE).unwrap_or_else(Instant::now);
        let terminals = &mut cx.global_mut::<Live>().terminals;
        if let Some((_, earlier, _)) = terminals.again.insert(session, (chat, reply, until)) {
            let _gone = earlier.send(Err(RpcError::Failed("Asked again".to_owned())));
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AGAIN_GRACE).await;
            let _gone = this.update(cx, Self::live_settle_again);
        })
        .detach();
        self.live_settle_again(cx);
        later(answer, cx)
    }

    /// Answer each terminal asked for again: shown once its machine says it runs and no tab
    /// shows it, refused once it ended or its wait is over.
    pub(in crate::workspace) fn live_settle_again(&mut self, cx: &mut Context<Self>) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if live.terminals.again.is_empty() {
            return;
        }
        let again = std::mem::take(&mut cx.global_mut::<Live>().terminals.again);
        let mut waiting = HashMap::new();
        let now = Instant::now();
        for (session, (chat, reply, until)) in again {
            if reply.is_closed() {
                continue;
            }
            let state = self
                .worker_of_session(session)
                .and_then(|key| self.workers.get(&key)?.sessions.get(&session))
                .map(|summary| summary.state);
            let terminals = &cx.global::<Live>().terminals;
            let free =
                !terminals.shown.contains(&session) && !terminals.coming.contains_key(&session);
            match state {
                Some(SessionState::Exited { .. }) => {
                    let _gone = reply.send(Err(RpcError::Failed("The terminal has ended".into())));
                }
                Some(_) if free => {
                    self.live_hold(session, &chat, cx);
                    cx.global_mut::<Live>().terminals.coming.insert(session, (chat, reply));
                }
                _ if now >= until => {
                    let _gone = reply.send(Err(RpcError::Failed("The terminal is gone".into())));
                }
                _ => {
                    waiting.insert(session, (chat, reply, until));
                }
            }
        }
        cx.global_mut::<Live>().terminals.again.extend(waiting);
        self.live_settle_terminals(cx);
    }

    /// `session` held for `chat` in its worker's items. A terminal is shown only through its
    /// item, whose view the workspace makes; one the worker opened itself has none until one
    /// is added here, and its open would wait for good.
    fn live_hold(&mut self, session: SessionId, chat: &str, cx: &mut Context<Self>) {
        let Some(key) = self.worker_of_session(session) else { return };
        if self.workers.get(&key).is_some_and(|w| w.doc.item_for_session(session).is_some()) {
            return;
        }
        let facts = std::collections::BTreeMap::from([(CHAT.to_owned(), chat.to_owned())]);
        let item =
            Item { id: ItemId::new(), kind: ItemKind::Terminal { session }, name: None, facts };
        self.propose(key, ItemOp::Add(item), cx);
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

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use slopty_core::SessionId;
    use slopty_proto::items::{ItemKind, ItemSync};
    use slopty_theme::Theme;

    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, link, mac, shell};

    /// A terminal the worker opened itself has no item, and a terminal shows only through its
    /// item: holding it for a chat adds one, once, carrying the chat. A session no worker has
    /// gets none.
    #[gpui::test]
    fn a_terminal_is_held_for_its_chat_once(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let opened = SessionId::new();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), vec![shell(opened)]);
        workspace.update(cx, |v, cx| {
            v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items: Vec::new() }, cx);
        });
        cx.run_until_parked();

        workspace.update(cx, |v, cx| {
            v.live_hold(opened, "chat-1", cx);
            v.live_hold(opened, "chat-1", cx);
            v.live_hold(SessionId::new(), "chat-1", cx);
        });
        let held: Vec<_> = workspace.read_with(cx, |v, _| {
            v.workers[&KEY]
                .doc
                .items()
                .map(|item| (item.kind, item.facts.get(super::CHAT).cloned()))
                .collect()
        });
        assert_eq!(held, [(ItemKind::Terminal { session: opened }, Some("chat-1".to_owned()))]);
    }

    /// A terminal a tab showed when the app last ran, asked for again by its id: one its
    /// machine says still runs is shown again for the chat, one that ended is refused at once,
    /// and one its machine has not spoken of yet is waited for.
    #[gpui::test]
    fn a_terminal_asked_for_again_is_shown_while_it_runs(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let (runs, ended, unknown) = (SessionId::new(), SessionId::new(), SessionId::new());
        let mut gone = shell(ended);
        gone.state = slopty_proto::terminal::SessionState::Exited { status: Some(0) };
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), vec![shell(runs), gone]);
        workspace.update(cx, |v, cx| {
            v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items: Vec::new() }, cx);
        });
        cx.run_until_parked();

        let _answers = workspace.update(cx, |v, cx| {
            cx.default_global::<super::Live>();
            [runs, ended, unknown].map(|session| v.live_open_again(session, "chat-1".into(), cx))
        });
        let (again, coming, held) = workspace.read_with(cx, |v, cx| {
            let terminals = &cx.global::<super::Live>().terminals;
            let again: Vec<SessionId> = terminals.again.keys().copied().collect();
            // Come, or already shown once its view was there.
            let coming: Vec<SessionId> =
                terminals.coming.keys().chain(&terminals.shown).copied().collect();
            let held: Vec<_> = v.workers[&KEY].doc.items().map(|item| item.kind).collect();
            (again, coming, held)
        });
        // The one that ended was answered at once: neither waited for nor shown.
        assert_eq!(again, [unknown], "only the one its machine never spoke of waits");
        assert_eq!(coming, [runs], "the one that runs is shown again");
        assert_eq!(held, [ItemKind::Terminal { session: runs }]);
    }
}
