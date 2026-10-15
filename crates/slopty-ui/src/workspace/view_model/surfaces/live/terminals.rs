//! A chat's terminals: `OpenTerminal` asks the chat's machine for a shell in its folder, and the
//! answer, once the shell's view is attached, is the session's id; the panel then shows that
//! view. A shell opened for a chat and still running when it is asked again (the app came back,
//! the session lived on in its worker) is found by its `chat` fact and shown again instead of a
//! new one. A chat whose agent runs in a terminal (Claude Code's TUI) shows that terminal
//! first, the same session the chat shows, so the person goes on in either. A project's run
//! script opens in a terminal of its own. `CloseTerminal` ends the shell, but only puts an
//! agent's terminal away.

use std::collections::{HashMap, HashSet};

use gpui::{App, Context, Focusable as _, SharedString, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_core::{ItemId, SessionId};
use slopty_proto::git::RunScript;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::terminal::{OpenSession, SessionState, TermSize};
use slopty_proto::thread::{AgentId, IntentId, Liveness};
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
    /// Agents' terminals asked for while their agent had exited, by chat: the start taking the
    /// agent up again in a terminal of its own, and the answer, given once the terminal is there.
    resuming: HashMap<String, (IntentId, Reply)>,
}

/// The program a session runs, as a tab names it: the command's last path part.
fn shell_of(command: &[String]) -> String {
    command.first().and_then(|program| program.rsplit('/').next()).unwrap_or_default().to_owned()
}

impl WorkspaceView {
    /// `OpenTerminal`: a shell for `params.chatId` on its machine, in its folder or
    /// `params.cwd`, sized `params.cols` × `params.rows` until its view fits it. With
    /// `params.agent`, the terminal the chat's agent runs in alone, its TUI on the session the
    /// chat shows, and never a shell in its place.
    pub(super) fn live_open_terminal(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        if params.get("agent").and_then(serde_json::Value::as_bool) == Some(true) {
            let chat = param(params, "chatId").unwrap_or_default().to_owned();
            let Some(session) = self.live_agent_terminal(&chat, cx) else {
                return self.live_resume_in_terminal(chat, cx);
            };
            self.live_hold(session, &chat, cx);
            let (reply, answer) = oneshot::channel();
            cx.global_mut::<Live>().terminals.coming.insert(session, (chat, reply));
            self.live_settle_terminals(cx);
            return later(answer, cx);
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
            && let Some(session) =
                self.live_agent_terminal(&chat, cx).or_else(|| self.live_kept_shell(key, &chat, cx))
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

    /// The terminal of `chat`'s agent asked for while it runs in none: an agent that exited (a
    /// Claude Code closed, its machine restarted) is taken up again on its own session, in a
    /// terminal of its own, and the answer waits for that terminal.
    fn live_resume_in_terminal(
        &mut self,
        chat: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        const NONE: &str =
            "Its agent has no terminal here, or that terminal is open in another tab";
        let Some((key, thread)) = self.chat_thread(&chat, cx) else {
            return now(Err(RpcError::Failed("The chat has not started yet".to_owned())));
        };
        let hub = self.thread_hub(key, cx);
        let start = match hub.read(cx).resume_start(thread) {
            Ok(Some(start)) if start.agent.is(AgentId::CLAUDE_CODE) => start,
            Ok(_) => return now(Err(RpcError::Failed(NONE.to_owned()))),
            Err(why) => return now(Err(RpcError::Failed(why))),
        };
        let (reply, answer) = oneshot::channel();
        let id = hub.update(cx, |hub, cx| hub.resume(thread, start, cx));
        if let Some((_, earlier)) =
            cx.global_mut::<Live>().terminals.resuming.insert(chat, (id, reply))
        {
            let _gone = earlier.send(Err(RpcError::Failed("Asked again".to_owned())));
        }
        later(answer, cx)
    }

    /// Answer each terminal asked of an agent taken up again: once it runs in its terminal, that
    /// terminal; once its start was turned down, why.
    pub(in crate::workspace) fn live_settle_resumes(&mut self, cx: &mut Context<Self>) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if live.terminals.resuming.is_empty() {
            return;
        }
        let resuming = std::mem::take(&mut cx.global_mut::<Live>().terminals.resuming);
        let mut waiting = HashMap::new();
        for (chat, (id, reply)) in resuming {
            if reply.is_closed() {
                continue;
            }
            if let Some(session) = self.live_agent_terminal(&chat, cx) {
                self.live_hold(session, &chat, cx);
                cx.global_mut::<Live>().terminals.coming.insert(session, (chat, reply));
                continue;
            }
            let refused = self.chat_thread(&chat, cx).and_then(|(key, thread)| {
                let hub = self.held_hub(key)?.read(cx);
                hub.refusals(thread).find(|r| r.id == id).map(|r| r.words.clone())
            });
            if let Some(words) = refused {
                let _gone = reply.send(Err(RpcError::Failed(words)));
                continue;
            }
            waiting.insert(chat, (id, reply));
        }
        cx.global_mut::<Live>().terminals.resuming.extend(waiting);
        self.live_settle_terminals(cx);
    }

    /// The terminal the agent of `chat` runs in (Claude Code's own TUI, on the same session the
    /// chat shows), still running and shown nowhere: a chat's terminal opens on it first, so the
    /// person can go on in the TUI and the GUI at once.
    fn live_agent_terminal(&self, chat: &str, cx: &Context<Self>) -> Option<SessionId> {
        let (key, thread) = self.chat_thread(chat, cx)?;
        let (_, hub) = self.hubs().find(|(at, _)| *at == key)?;
        let row = hub.read(cx).threads().rows().rows.get(&thread)?;
        // An agent that exited left its terminal to whatever runs there now, maybe another
        // session: it is taken up again in a terminal of its own instead.
        if row.status.liveness != Liveness::Live {
            return None;
        }
        let session = row.terminal?;
        let terminals = &cx.try_global::<Live>()?.terminals;
        let free = !terminals.shown.contains(&session) && !terminals.coming.contains_key(&session);
        let alive = self
            .workers
            .get(&key)?
            .sessions
            .get(&session)
            .is_some_and(|s| !matches!(s.state, SessionState::Exited { .. }));
        (free && alive).then_some(session)
    }

    /// `session` held for `chat` in its worker's items. A terminal is shown only through its
    /// item, whose view the workspace makes; an agent's terminal the worker opened itself (a
    /// start, a seat) has none until one is added here, and its open would wait for good.
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

    /// Whether `session` is the terminal an agent runs in.
    fn agent_runs_in(&self, session: SessionId, cx: &App) -> bool {
        self.hubs().any(|(_, hub)| {
            let rows = &hub.read(cx).threads().rows().rows;
            rows.values().any(|row| row.terminal == Some(session))
        })
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
        // An agent's own terminal is only put away: closing its tab never ends the agent.
        if self.agent_runs_in(session, cx) {
            return Ok(serde_json::json!({}));
        }
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

    /// An agent's terminal the worker opened itself has no item, and a terminal shows only
    /// through its item: holding it for a chat adds one, once, carrying the chat. A session no
    /// worker has gets none.
    #[gpui::test]
    fn an_agent_s_terminal_is_held_for_its_chat_once(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let agent = SessionId::new();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), vec![shell(agent)]);
        workspace.update(cx, |v, cx| {
            v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items: Vec::new() }, cx);
        });
        cx.run_until_parked();

        workspace.update(cx, |v, cx| {
            v.live_hold(agent, "chat-1", cx);
            v.live_hold(agent, "chat-1", cx);
            v.live_hold(SessionId::new(), "chat-1", cx);
        });
        let held: Vec<_> = workspace.read_with(cx, |v, _| {
            v.workers[&KEY]
                .doc
                .items()
                .map(|item| (item.kind, item.facts.get(super::CHAT).cloned()))
                .collect()
        });
        assert_eq!(held, [(ItemKind::Terminal { session: agent }, Some("chat-1".to_owned()))]);
    }
}
