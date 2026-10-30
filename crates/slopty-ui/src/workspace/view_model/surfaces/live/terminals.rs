//! A chat's terminals: `OpenTerminal` asks the chat's machine for a shell in its folder, and the
//! answer, once the shell's view is attached, is the session's id; the panel then shows that
//! view. A shell opened for a chat and still running when it is asked again (the app came back,
//! the session lived on in its worker) is found by its `chat` fact and shown again instead of a
//! new one. A script (an agent's installer) opens in a terminal of its own. `CloseTerminal`
//! ends the shell. A tab the app showed when it last ran asks for its terminal again by
//! its id (`terminalId`): shown once its machine says it still runs, refused once it ended or
//! [`AGAIN_GRACE`] passed with its machine silent of it.
//!
//! The app's links to its machines, and their items, come after the app does, and a tab opened
//! at launch asks ahead of them. A shell is opened only once the chat's machine is linked and
//! its items are here, so the one kept for the chat is found rather than a second one opened
//! beside it; until then the open waits, and it is refused as out of reach (or as having no
//! machine) only once [`AGAIN_GRACE`] passed.

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

use super::{Live, LiveLook, LiveView, later, param};
use crate::workspace::WorkspaceView;

/// The fact a shell opened for the interface carries: the chat it was opened for.
const CHAT: &str = crate::workspace::CHAT_FACT;

/// What [`CHAT`] says of a terminal that is a session of its own, an agent's or a plain shell:
/// `terminal:<session>`, which is also the plain terminal's chat in the sidebar
/// (`docs/decisions/ui.md`, "A session is its terminal").
pub const TERMINAL_CHAT: &str = "terminal:";

/// The [`CHAT`] fact of session terminal `session`.
fn terminal_chat(session: SessionId) -> String {
    format!("{TERMINAL_CHAT}{session}")
}

/// Where an open's answer goes.
type Reply = oneshot::Sender<Result<RpcReply, RpcError>>;

/// How long a terminal asked for again by its id, or a shell asked for ahead of its machine's
/// link and items, is waited for: the app's links to its machines, and their sessions and
/// items, come back after the app does.
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
    /// Shells asked for before the chat's machine was linked and its items here, in the order
    /// asked: the call, the answer, and until when its machine is waited for.
    early: Vec<(serde_json::Value, Reply, Instant)>,
    /// Plain terminals started as sessions of their own (`slopty.StartSession`), asked of a
    /// machine: answered with their chat once it says their session.
    started: HashMap<(WorkerKey, RequestId), Reply>,
    /// Terminals to be marked as sessions of their own ([`TERMINAL_CHAT`]) once their item is
    /// here.
    adopt: HashSet<SessionId>,
}

/// The program a session runs, as a tab names it: the command's last path part.
fn shell_of(command: &[String]) -> String {
    command.first().and_then(|program| program.rsplit('/').next()).unwrap_or_default().to_owned()
}

impl WorkspaceView {
    /// `OpenTerminal`: a shell for `params.chatId` on its machine, in its folder or
    /// `params.cwd`, sized `params.cols` × `params.rows` until its view fits it. With
    /// `params.terminalId`, that terminal again; with `params.session` too, a session's own
    /// terminal, shown by the one panel that shows sessions, one at a time.
    pub(super) fn live_open_terminal(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        if let Some(session) = param(params, "terminalId").and_then(|id| id.parse().ok()) {
            let chat = param(params, "chatId").unwrap_or_default().to_owned();
            let own = params.get("session").and_then(serde_json::Value::as_bool) == Some(true);
            return self.live_open_again(session, chat, own, cx);
        }
        self.live_open_session(params, None, cx)
    }

    /// A terminal for `params.chatId` as [`Self::live_open_terminal`] opens one, running `script`
    /// on `key` where one is named, in a fresh terminal of its own. A shell for the chat waits
    /// for its machine's link and items ([`Self::live_settle_opens`]); a script runs at once.
    pub(in crate::workspace) fn live_open_session(
        &mut self,
        params: &serde_json::Value,
        script: Option<(WorkerKey, RunScript)>,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (reply, answer) = oneshot::channel();
        if script.is_some() {
            self.live_open_now(params, script, reply, cx);
            return later(answer, cx);
        }
        let now = cx.background_executor().now();
        let until = now.checked_add(AGAIN_GRACE).unwrap_or(now);
        cx.default_global::<Live>().terminals.early.push((params.clone(), reply, until));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AGAIN_GRACE).await;
            let _gone = this.update(cx, Self::live_settle_opens);
        })
        .detach();
        self.live_settle_opens(cx);
        later(answer, cx)
    }

    /// Open each shell asked for ahead of its machine whose machine is linked and whose items
    /// are here now, or whose wait is over: the open then finds the shell kept for its chat, or
    /// opens one, or says why it can't.
    pub(in crate::workspace) fn live_settle_opens(&mut self, cx: &mut Context<Self>) {
        let Some(live) = cx.try_global::<Live>() else { return };
        if live.terminals.early.is_empty() {
            return;
        }
        let early = std::mem::take(&mut cx.global_mut::<Live>().terminals.early);
        let now = cx.background_executor().now();
        let mut waiting = Vec::new();
        for (params, reply, until) in early {
            if reply.is_closed() {
                continue;
            }
            if now < until && !self.live_open_ready(&params, cx) {
                waiting.push((params, reply, until));
                continue;
            }
            self.live_open_now(&params, None, reply, cx);
        }
        let terminals = &mut cx.global_mut::<Live>().terminals;
        waiting.append(&mut terminals.early);
        terminals.early = waiting;
    }

    /// Whether the shell `params` asks for can be opened now: the chat's machine is linked and
    /// its items are here. A chat that names no machine the workspace knows yet opens on the
    /// machine in context once every machine is.
    fn live_open_ready(&self, params: &serde_json::Value, cx: &Context<Self>) -> bool {
        let ready = |key: &WorkerKey| {
            self.workers.get(key).is_some_and(|w| w.is_linked() && !w.awaiting_snapshot)
        };
        match self.live_chat_place(params, cx) {
            Some((key, _)) => ready(&key),
            None => !self.workers.is_empty() && self.workers.keys().all(ready),
        }
    }

    /// [`Self::live_open_session`] now, its answer to `reply`.
    fn live_open_now(
        &mut self,
        params: &serde_json::Value,
        script: Option<(WorkerKey, RunScript)>,
        reply: Reply,
        cx: &mut Context<Self>,
    ) {
        let chat = param(params, "chatId").unwrap_or_default().to_owned();
        let place = match &script {
            Some((key, script)) => Some((*key, Some(script.cwd.clone()))),
            None => self.live_place(params, cx),
        };
        let Some((key, cwd)) = place else {
            let why = "no machine to open a terminal on".to_owned();
            let _gone = reply.send(Err(RpcError::Failed(why)));
            return;
        };
        let cwd = param(params, "cwd").filter(|_| script.is_none()).map(str::to_owned).or(cwd);
        if script.is_none()
            && let Some(session) = self.live_kept_shell(key, &chat, cx)
        {
            self.live_hold(session, &chat, cx);
            cx.default_global::<Live>().terminals.coming.insert(session, (chat, reply));
            self.live_settle_terminals(cx);
            return;
        }
        if !self.reachable_for(key, "the terminal", cx) {
            let name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
            let _gone = reply.send(Err(RpcError::Failed(format!("{name} is out of reach"))));
            return;
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
        cx.default_global::<Live>().terminals.asked.insert((key, request), (chat, reply));
        tracing::debug!(%key, request, cwd = ?spec.cwd, "a terminal for the interface");
        self.send(key, ClientMsg::OpenSession { request, spec });
    }

    /// Terminal `session` for `chat` again, which a tab showed when the app last ran: answered
    /// once its machine says it still runs, refused once it ended or [`AGAIN_GRACE`] passed.
    /// A session's own terminal (`own`) is shown by the session panel alone, one at a time, so
    /// a tab of it already shown there does not hold it.
    fn live_open_again(
        &mut self,
        session: SessionId,
        chat: String,
        own: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (reply, answer) = oneshot::channel();
        let now = cx.background_executor().now();
        let until = now.checked_add(AGAIN_GRACE).unwrap_or(now);
        let terminals = &mut cx.global_mut::<Live>().terminals;
        if own {
            terminals.shown.remove(&session);
        }
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
        let now = cx.background_executor().now();
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

    /// The terminal an agent runs in now, each with its thread's chat: the threads' rows name
    /// them (`ThreadMeta::TERMINAL_FACT`). Only a session's own thread is its terminal's: a
    /// subagent runs inside its agent's TUI (the shell shows it in its agent's panes), so a
    /// subagent's row that names the terminal too never takes it from its agent.
    fn live_agent_terminals(&self, cx: &App) -> HashMap<SessionId, String> {
        self.hubs()
            .flat_map(|(_, hub)| {
                hub.read(cx)
                    .threads()
                    .rows()
                    .rows
                    .values()
                    .filter(|row| row.parent.is_none())
                    .filter_map(|row| Some((row.terminal()?, row.id.to_string())))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The terminal each session is, by chat: an agent's session by the one its TUI runs in
    /// now, a plain terminal ([`TERMINAL_CHAT`]) by its own, while it runs.
    pub(in crate::workspace) fn live_session_terminals(
        &self,
        cx: &App,
    ) -> std::collections::BTreeMap<String, String> {
        let agents = self.live_agent_terminals(cx);
        let mut out: std::collections::BTreeMap<String, String> =
            agents.iter().map(|(session, chat)| (chat.clone(), session.to_string())).collect();
        for (session, chat) in self.live_plain_terminals() {
            if !agents.contains_key(&session) {
                out.insert(chat, session.to_string());
            }
        }
        out
    }

    /// The plain terminals that are sessions of their own and still run, each with its chat.
    fn live_plain_terminals(&self) -> Vec<(SessionId, String)> {
        let mut out = Vec::new();
        for w in self.workers.values() {
            for item in w.doc.items() {
                let ItemKind::Terminal { session } = item.kind else { continue };
                let Some(chat) =
                    item.facts.get(CHAT).filter(|chat| chat.starts_with(TERMINAL_CHAT))
                else {
                    continue;
                };
                let running = w
                    .sessions
                    .get(&session)
                    .is_some_and(|s| !matches!(s.state, SessionState::Exited { .. }));
                if running {
                    out.push((session, chat.clone()));
                }
            }
        }
        out
    }

    /// Mark the terminals agents run in, and the plain terminals started as sessions, as
    /// sessions of their own ([`TERMINAL_CHAT`]), so each outlives the agent in it as the plain
    /// terminal it then is. A shell opened for a chat's dock keeps its chat.
    pub(in crate::workspace) fn live_adopt_terminals(&mut self, cx: &mut Context<Self>) {
        let asked: HashSet<SessionId> =
            cx.try_global::<Live>().map(|live| live.terminals.adopt.clone()).unwrap_or_default();
        let mut sessions = asked.clone();
        sessions.extend(self.live_agent_terminals(cx).into_keys());
        for session in sessions {
            let Some(key) = self.worker_of_session(session) else { continue };
            let Some(item) = self.workers.get(&key).and_then(|w| w.doc.item_for_session(session))
            else {
                continue;
            };
            let (id, chat) = (item.id, terminal_chat(session));
            // A shell of a chat's dock that an agent was started in stays the dock's.
            let keep = match item.facts.get(CHAT) {
                Some(fact) => *fact == chat || !asked.contains(&session),
                None => false,
            };
            if asked.contains(&session) {
                cx.global_mut::<Live>().terminals.adopt.remove(&session);
            }
            if !keep {
                let op = ItemOp::SetFact { id, key: CHAT.to_owned(), value: Some(chat) };
                self.propose(key, op, cx);
            }
        }
    }

    /// The plain terminals opened as sessions of their own, as the sidebar's chats and their
    /// sessions: each one running, under its title, in the project its folder is in. One an
    /// agent runs in now is that agent's session instead. At rest they stay quiet
    /// (`SessionStatus::Idle`).
    pub(in crate::workspace) fn live_terminal_rows(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        cx: &App,
    ) -> Vec<(slopty_view_proto::Chat, slopty_view_proto::Session)> {
        let agents = self.live_agent_terminals(cx);
        let mut out = Vec::new();
        for (key, w) in &self.workers {
            let device = key.to_string();
            for item in w.doc.items() {
                let ItemKind::Terminal { session } = item.kind else { continue };
                let Some(id) = item.facts.get(CHAT).filter(|chat| chat.starts_with(TERMINAL_CHAT))
                else {
                    continue;
                };
                if agents.contains_key(&session) {
                    continue;
                }
                let Some(summary) = w.sessions.get(&session) else { continue };
                if matches!(summary.state, SessionState::Exited { .. }) {
                    continue;
                }
                let created = crate::view_model::rows::time(summary.started_ms);
                let place =
                    self.view_place(&device, summary.cwd.as_deref(), summary.repo.as_deref());
                let title = item.name.clone().or_else(|| {
                    Some(summary.title.trim().to_owned()).filter(|title| !title.is_empty())
                });
                let chat = slopty_view_proto::Chat {
                    id: id.clone(),
                    device_id: device.clone(),
                    title,
                    archived: false,
                    cwd: summary.cwd.clone(),
                    branch: summary.branch.clone(),
                    checkout_id: None,
                    source_context: None,
                    config: None,
                    last_message_preview: None,
                    last_message_at: None,
                    created_at: created,
                    harness_session_id: None,
                    harness_session_cwd: None,
                    space_id: place.map(|path| self.view_space_id(&device, &path)),
                    last_seen_at: None,
                    parent_chat_id: None,
                    subagent: false,
                    in_parent: false,
                    draft: None,
                };
                let row = slopty_view_proto::Session {
                    last_completed_turn: None,
                    chat_id: id.clone(),
                    device_id: device.clone(),
                    status: slopty_view_proto::SessionStatus::Idle,
                    started_at: None,
                    updated_at: now,
                    running_subagents: 0,
                    status_since: None,
                    waiting_on: None,
                };
                out.push((chat, row));
            }
        }
        out
    }

    /// `slopty.StartSession` with a shell: a plain terminal on `key` in `cwd`, a session of its
    /// own, answered with its chat (`terminal:<session>`) once its machine says its session.
    pub(in crate::workspace) fn live_start_terminal(
        &self,
        key: WorkerKey,
        cwd: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (reply, answer) = oneshot::channel();
        let spec = OpenSession {
            size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
            cwd: Some(cwd),
            command: Vec::new(),
            env: Vec::new(),
            title: None,
            attach: false,
        };
        let request = self.next_open.get();
        self.next_open.set(request.wrapping_add(1));
        cx.default_global::<Live>().terminals.started.insert((key, request), reply);
        self.send(key, ClientMsg::OpenSession { request, spec });
        later(answer, cx)
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
        if !cx.has_global::<Live>() {
            return;
        }
        if let Some(reply) = cx.global_mut::<Live>().terminals.started.remove(&(key, request)) {
            let chat = terminal_chat(session);
            self.live_hold(session, &chat, cx);
            cx.global_mut::<Live>().terminals.adopt.insert(session);
            self.live_adopt_terminals(cx);
            let _gone = reply.send(Ok(RpcReply::Value(serde_json::json!({ "chatId": chat }))));
            return;
        }
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
        if !live.terminals.asked.contains_key(&(key, request))
            && !live.terminals.started.contains_key(&(key, request))
        {
            return;
        }
        let terminals = &mut cx.global_mut::<Live>().terminals;
        if let Some((_, reply)) = terminals.asked.remove(&(key, request)) {
            let _gone = reply.send(Err(RpcError::Failed(message.to_owned())));
        }
        if let Some(reply) = terminals.started.remove(&(key, request)) {
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
    use std::collections::BTreeMap;
    use std::time::Duration;

    use gpui::{AppContext as _, TestAppContext, VisualTestContext};
    use slopty_core::{ItemId, SessionId};
    use slopty_proto::ClientMsg;
    use slopty_proto::items::{Item, ItemKind, ItemSync};
    use slopty_theme::Theme;
    use slopty_view_rpc::methods;

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

    /// A subagent runs inside its agent's TUI, and its row may name that terminal too (its thread
    /// begun from its agent's record). The terminal stays its agent's session whichever of the
    /// two threads' ids sorts first, so the shell, following the terminal, never moves from the
    /// session to its subagent; and once the agent left it, a subagent's stale word for it makes
    /// it no agent's.
    #[gpui::test]
    fn a_subagent_never_takes_its_agents_terminal(cx: &mut TestAppContext) {
        use slopty_core::WallMs;
        use slopty_proto::thread::wire::TableFrame;
        use slopty_proto::thread::{Cursor, Link, ThreadId, ThreadMeta};

        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let terminal = SessionId::new();
        let id = |n: u8| -> ThreadId {
            format!("00000000-0000-8000-8000-0000000000{n:02}").parse().expect("a thread id")
        };
        let row = |thread: ThreadId, parent: Option<ThreadId>, in_terminal: bool| {
            let mut state = crate::conversation::thread::fixtures::empty();
            state.meta.id = thread;
            state.meta.parent = parent.map(|thread| Link {
                thread,
                item: slopty_proto::thread::ItemId("toolu_1".to_owned()),
            });
            if in_terminal {
                state.meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), terminal.to_string());
            }
            state.row(WallMs::ZERO)
        };
        workspace.update(cx, |v, cx| v.threads_linked(KEY, cx));
        let mut seq = 0;
        let mut terminals = |rows: Vec<_>, cx: &mut VisualTestContext| {
            seq = u64::saturating_add(seq, 1);
            workspace.update(cx, |v, cx| {
                let cursor = Cursor { epoch: 1, seq };
                v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
                v.live_session_terminals(cx)
            })
        };
        // The subagent's id sorting after its agent's, then before it.
        for (agent, subagent) in [(id(1), id(2)), (id(2), id(1))] {
            let rows = vec![row(agent, None, true), row(subagent, Some(agent), true)];
            assert_eq!(
                terminals(rows, cx),
                BTreeMap::from([(agent.to_string(), terminal.to_string())]),
                "the terminal is the agent's session, not its subagent's"
            );
        }
        let (agent, subagent) = (id(1), id(2));
        let left = vec![row(agent, None, false), row(subagent, Some(agent), true)];
        assert_eq!(terminals(left, cx), BTreeMap::new(), "no agent's once its agent left it");
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
            [runs, ended, unknown]
                .map(|session| v.live_open_again(session, "chat-1".into(), false, cx))
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

    /// The shells waiting for their machine, and those come or shown, as `workspace` has them.
    fn opens(
        workspace: &gpui::Entity<WorkspaceView>,
        cx: &VisualTestContext,
    ) -> (usize, Vec<SessionId>) {
        workspace.read_with(cx, |_, cx| {
            let terminals = &cx.global::<super::Live>().terminals;
            let come = terminals.coming.keys().chain(&terminals.shown).copied().collect();
            (terminals.early.len(), come)
        })
    }

    /// A tab opened at launch asks for its chat's shell before the app's link to the machine is
    /// up and before the machine's items are here. It waits for both, then gets the shell kept
    /// for its chat, never a second one opened beside it.
    #[gpui::test]
    fn a_shell_asked_for_ahead_of_its_machine_is_the_one_kept_for_its_chat(
        cx: &mut TestAppContext,
    ) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let kept = SessionId::new();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let ask = serde_json::json!({ "chatId": "chat-1", "cols": 80, "rows": 24 });
        let _answer =
            workspace.update(cx, |v, cx| v.live_surface_call(methods::OPEN_TERMINAL, &ask, cx));
        cx.run_until_parked();
        assert_eq!(opens(&workspace, cx), (1, Vec::new()), "waits for the link");

        let mut rx = link(&workspace, cx, mac(|_| {}), vec![shell(kept)]);
        assert_eq!(opens(&workspace, cx), (1, Vec::new()), "and for the machine's items");

        let facts = BTreeMap::from([(super::CHAT.to_owned(), "chat-1".to_owned())]);
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session: kept },
            name: None,
            facts,
        };
        workspace.update(cx, |v, cx| {
            v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items: vec![item] }, cx);
        });
        cx.run_until_parked();
        assert_eq!(opens(&workspace, cx), (0, vec![kept]), "the shell kept for the chat");
        let opened = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|msg| matches!(msg, ClientMsg::OpenSession { .. }))
            .count();
        assert_eq!(opened, 0, "no second shell");
    }

    /// A shell asked for on a machine whose link never comes up waits out [`super::AGAIN_GRACE`]
    /// and is then refused, saying why.
    #[gpui::test]
    fn a_shell_whose_machine_never_links_is_refused_after_the_grace(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let ask = serde_json::json!({ "chatId": "chat-1" });
        let answer =
            workspace.update(cx, |v, cx| v.live_surface_call(methods::OPEN_TERMINAL, &ask, cx));
        let mut answer = answer.expect("a live surface's call");
        cx.executor().advance_clock(super::AGAIN_GRACE.saturating_sub(Duration::from_secs(1)));
        cx.run_until_parked();
        assert!(futures::FutureExt::now_or_never(&mut answer).is_none(), "still waits");
        assert_eq!(opens(&workspace, cx), (1, Vec::new()));

        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        let refused =
            futures::FutureExt::now_or_never(answer).expect("answered once the wait is over");
        let Err(slopty_view_rpc::RpcError::Failed(why)) = refused else {
            panic!("refused, not opened");
        };
        assert_eq!(why, "studio is out of reach");
        assert_eq!(opens(&workspace, cx), (0, Vec::new()), "and waits no more");
    }
}
