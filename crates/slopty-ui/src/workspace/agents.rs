//! Coding agents in terminals: what the worker says about them, the header badge, the banner
//! when the human is away, and the count of the ones waiting on the human.

use gpui::accesskit::Role;
use gpui::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, SystemNotification, Window, div, px,
};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason, SessionAgent};
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::SessionSummary;
use slopty_theme::alpha;

use super::actions::NextAttention;
use super::tile::Chrome;
use super::{Finished, WorkspaceEvent, WorkspaceView};
use crate::a11y::tab_stop;
use crate::chrome_text::ChromeText;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::Status;

/// An agent waiting on the human: where, and the tile that shows it, if one does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Waiting {
    /// The worker it runs on.
    pub worker: WorkerKey,
    /// Its tile here.
    pub tile: Option<TileRef>,
    /// Its session.
    pub session: SessionId,
}

/// A banner's title: what the agent is doing, led by the tile's name when the human gave it
/// one, so a banner from several agents says which tile it is about.
#[must_use]
pub fn banner_title(name: Option<&str>, what: &str) -> String {
    match name {
        Some(name) => format!("{name} · {what}"),
        None => what.to_owned(),
    }
}

/// A program's notification as a banner: its title led by the tile's name, "Terminal" when
/// the protocol carried no title (OSC 9), and its body.
#[must_use]
pub fn program_banner(name: Option<&str>, title: &str, body: &str) -> (String, String) {
    let title = if title.trim().is_empty() { "Terminal" } else { title.trim() };
    (banner_title(name, title), body.trim().to_owned())
}

/// Whether the agent is waiting on the human (an idle prompt is not worth an outline).
pub(super) fn needs_human(agent: &AgentEvent) -> bool {
    matches!(&agent.status, AgentStatus::Blocked(why) if *why != BlockReason::IdlePrompt)
}

/// One short line for an agent's state: the badge text, the picker's status column.
#[must_use]
pub fn agent_status_text(agent: &AgentEvent) -> String {
    let detail = agent.detail.as_deref().filter(|d| !d.is_empty());
    match &agent.status {
        AgentStatus::None => String::new(),
        AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt) => "Idle".to_owned(),
        AgentStatus::Working => detail.unwrap_or("Working").to_owned(),
        AgentStatus::Tool { tool } => detail.unwrap_or(tool).to_owned(),
        AgentStatus::Blocked(BlockReason::Permission { tool }) => {
            format!("Allow {}?", detail.unwrap_or(tool))
        }
        AgentStatus::Blocked(BlockReason::Question) => {
            detail.map_or_else(|| "Has a question".to_owned(), |d| format!("Asks: {d}"))
        }
        AgentStatus::Blocked(BlockReason::Elicitation) => {
            detail.map_or_else(|| "Needs input".to_owned(), |d| format!("Needs input: {d}"))
        }
        AgentStatus::Done => {
            detail.map_or_else(|| "Turn finished".to_owned(), |d| format!("Done: {d}"))
        }
    }
}

impl WorkspaceView {
    /// A worker observed a coding agent's state in a session.
    pub fn agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        let session = event.session;
        if let Some(view) = self.terminals.get(&session) {
            let status = (event.status != AgentStatus::None).then(|| event.status.clone());
            view.update(cx, |v, cx| v.set_agent_status(status, cx));
        }
        if event.status == AgentStatus::None {
            self.agents.remove(&session);
        } else {
            let attention = event.attention;
            let needs = needs_human(&event);
            if attention {
                self.notify_system(&event, cx);
            } else if !needs {
                cx.dismiss_system_notification(&session.to_string());
            }
            self.agents.insert(session, event);
            if attention {
                cx.emit(WorkspaceEvent::Attention(session));
            }
        }
        self.update_awake(cx);
        self.count_needs_you(cx);
        self.update_run_targets(cx);
        cx.notify();
    }

    /// What the worker's summaries say runs in each session, taken before any `Agent` event
    /// arrives, so a tile shows its agent's badge, and offers the hooks only when they are not
    /// what the worker reads it from, from the first frame after a connect. A session that
    /// already has a live event keeps it: the event is newer than any summary.
    pub(super) fn seed_agents<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a SessionSummary>,
        cx: &mut Context<Self>,
    ) {
        let mut seeded = false;
        for summary in sessions {
            let Some(SessionAgent { kind, status, source }) = summary.agent.clone() else {
                continue;
            };
            if status == AgentStatus::None || self.agents.contains_key(&summary.id) {
                continue;
            }
            if let Some(view) = self.terminals.get(&summary.id) {
                let status = status.clone();
                view.update(cx, |v, cx| v.set_agent_status(Some(status), cx));
            }
            self.agents.insert(
                summary.id,
                AgentEvent {
                    session: summary.id,
                    kind,
                    status,
                    agent_session: None,
                    detail: None,
                    attention: false,
                    source,
                },
            );
            seeded = true;
        }
        if seeded {
            self.update_awake(cx);
            self.count_needs_you(cx);
            self.update_run_targets(cx);
        }
    }

    /// Hold the device awake while any agent works; let go when none does.
    pub(super) fn update_awake(&mut self, cx: &Context<Self>) {
        let working = self
            .agents
            .values()
            .any(|a| matches!(a.status, AgentStatus::Working | AgentStatus::Tool { .. }));
        if !working {
            self.awake = None;
        } else if self.awake.is_none() {
            let acquisition = cx.prevent_idle_sleep("Slopty agent working");
            self.awake = Some(cx.spawn(async move |_this, _cx| match acquisition.await {
                Ok(guard) => {
                    let _guard = guard;
                    std::future::pending::<()>().await;
                }
                Err(e) => tracing::warn!(error = %e, "idle sleep prevention"),
            }));
        }
    }

    /// The name of the tile showing `session`, for a banner.
    fn session_name(&self, session: SessionId) -> Option<&str> {
        self.tile_of_session(session).and_then(|t| self.item(t)).and_then(|i| i.name.as_deref())
    }

    /// A banner through the notification centre when the human is not looking at the app.
    fn notify_system(&self, event: &AgentEvent, cx: &Context<Self>) {
        let active = cx.active_window().is_some();
        tracing::debug!(active, session = %event.session, "agent banner");
        if active {
            return;
        }
        let title = match &event.status {
            AgentStatus::Blocked(BlockReason::Permission { tool }) if tool.is_empty() => {
                "Claude needs permission".to_owned()
            }
            AgentStatus::Blocked(BlockReason::Permission { tool }) => {
                format!("Claude wants to use {tool}")
            }
            AgentStatus::Blocked(BlockReason::Question) => "Claude has a question".to_owned(),
            AgentStatus::Blocked(BlockReason::Elicitation) => "Claude needs input".to_owned(),
            AgentStatus::Done => "Claude finished".to_owned(),
            _ => agent_status_text(event),
        };
        let title = banner_title(self.session_name(event.session), &title);
        let body = event.detail.clone().filter(|d| !d.is_empty()).unwrap_or_default();
        cx.show_system_notification(SystemNotification {
            tag: event.session.to_string().into(),
            title: title.into(),
            body: body.into(),
            actions: Vec::new(),
        });
    }

    /// A program in a terminal asked for a desktop notification (OSC 9 / 777 / 99): the same
    /// banner an agent gets when the human is not looking, and the Dock bounce either way.
    pub(super) fn notify_program(
        &self,
        session: SessionId,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        if cx.active_window().is_none() {
            let (title, body) = program_banner(self.session_name(session), title, body);
            cx.show_system_notification(SystemNotification {
                tag: session.to_string().into(),
                title: title.into(),
                body: body.into(),
                actions: Vec::new(),
            });
        }
        cx.emit(WorkspaceEvent::Attention(session));
    }

    /// Sessions whose agent is waiting on the human: those with a tile in reading order
    /// (workspace, column, tile) so ⌘⇧A walks the strip predictably, then those the server
    /// reported on a worker with no tile for them here.
    pub(super) fn needs_you(&self) -> Vec<Waiting> {
        let mut shown: Vec<(Option<slopty_client::layout::Pos>, Waiting)> = self
            .items()
            .filter_map(|(worker, i)| match i.kind {
                ItemKind::Terminal { session } => {
                    Some(Waiting { worker, tile: Some(TileRef { worker, item: i.id }), session })
                }
                _ => None,
            })
            .filter(|w| self.agent_state(w.session).is_some_and(needs_human))
            .map(|w| (w.tile.and_then(|t| self.layout.position(t)), w))
            .collect();
        shown.sort_by_key(|(pos, w)| {
            (pos.map(|p| (p.workspace, p.column, p.tile)), w.tile.map(|t| t.item))
        });
        let mut unshown: Vec<Waiting> = self
            .server_agents
            .iter()
            .filter(|(session, (_, agent))| {
                needs_human(agent) && self.tile_of_session(**session).is_none()
            })
            .map(|(session, (worker, _))| Waiting {
                worker: *worker,
                tile: None,
                session: *session,
            })
            .collect();
        unshown.sort_by_key(|w| (w.worker, w.session));
        shown.into_iter().map(|(_, w)| w).chain(unshown).collect()
    }

    /// What is known about a session's agent: the worker's own word while its link is up,
    /// else the server's.
    pub(super) fn agent_state(&self, session: SessionId) -> Option<&AgentEvent> {
        self.agents.get(&session).or_else(|| self.server_agents.get(&session).map(|(_, a)| a))
    }

    /// The server relayed an agent's change on `worker`. It counts toward the agents that need
    /// the human whether or not the worker's own link is up or a tile shows the session, and
    /// raises the banner when no link of this client's would.
    pub fn server_agent_event(
        &mut self,
        worker: WorkerKey,
        event: AgentEvent,
        cx: &mut Context<Self>,
    ) {
        let session = event.session;
        let linked = self.workers.get(&worker).is_some_and(|w| w.link.is_some());
        if event.status == AgentStatus::None {
            self.server_agents.remove(&session);
        } else {
            if event.attention && !linked {
                self.notify_system(&event, cx);
                cx.emit(WorkspaceEvent::Attention(session));
            }
            self.server_agents.insert(session, (worker, event));
        }
        self.count_needs_you(cx);
        cx.notify();
    }

    /// The server says a session ended.
    pub fn server_session_closed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.server_agents.remove(&session).is_some() {
            self.count_needs_you(cx);
            cx.notify();
        }
    }

    /// Drop what the server said about agents: on `worker` (gone), or everywhere (`None`,
    /// the server was disconnected).
    pub fn forget_server_agents(&mut self, worker: Option<WorkerKey>, cx: &mut Context<Self>) {
        let before = self.server_agents.len();
        self.server_agents.retain(|_, (w, _)| worker.is_some_and(|gone| *w != gone));
        if self.server_agents.len() != before {
            self.count_needs_you(cx);
            cx.notify();
        }
    }

    /// How many agents are waiting on the human, on every worker.
    #[must_use]
    pub fn needs_you_count(&self) -> usize {
        self.needs_you().len()
    }

    /// Agents waiting on the human on one worker.
    #[must_use]
    pub fn needs_you_on(&self, worker: WorkerKey) -> usize {
        self.needs_you().iter().filter(|w| w.worker == worker).count()
    }

    /// Tell the app the count (the Dock badge).
    pub(super) fn count_needs_you(&self, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::NeedsYou(self.needs_you_count()));
    }

    /// ⌘⇧A: reveal and focus the next terminal whose agent is waiting on the human, cycling
    /// from the focused tile.
    pub fn next_attention(
        &mut self,
        _: &NextAttention,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let waiting = self.needs_you();
        let Some(first) = waiting.first() else { return };
        let next = self
            .focused()
            .and_then(|focused| waiting.iter().position(|w| w.tile == Some(focused)))
            .and_then(|i| waiting.iter().cycle().nth(i.saturating_add(1)))
            .unwrap_or(first);
        match next.tile {
            Some(_) => self.reveal_session(next.session, cx),
            None => self.show_untiled(next.worker, next.session, cx),
        }
    }

    /// An agent that needs the human in a session with no tile here: give it one, which the
    /// worker then syncs to every client. A worker this client cannot reach says so instead.
    pub(super) fn show_untiled(
        &mut self,
        worker: WorkerKey,
        session: SessionId,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get(&worker) else { return };
        if w.link.is_none() {
            let text = format!("{} is not reachable from here", w.name);
            self.show_notice(text, cx);
            return;
        }
        let item = slopty_proto::items::Item {
            id: slopty_core::ItemId::new(),
            kind: ItemKind::Terminal { session },
            sleeping: false,
            name: None,
        };
        self.propose(worker, slopty_proto::items::ItemOp::Upsert(item), cx);
        self.pending_focus = Some(session);
    }

    /// A shell command ended in `session`. Long enough, and in a tile the human is not on, it
    /// earns a header badge, cleared when the tile is focused.
    pub fn command_finished(&mut self, session: SessionId, done: Finished, cx: &mut Context<Self>) {
        let watched = self.tile_of_session(session).is_some_and(|t| self.focused() == Some(t));
        let slow = done.elapsed >= self.slow_command;
        tracing::info!(%session, watched, slow, elapsed = ?done.elapsed, "command finished");
        if watched || !slow {
            return;
        }
        self.finished.insert(session, done);
        cx.notify();
    }

    /// Ask `worker` to register `slopty hook` for its Claude Code. Offered once per run.
    pub fn install_hooks(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        if let Some(w) = self.workers.get_mut(&worker) {
            w.hooks_offered = true;
            w.send(ClientMsg::InstallHooks);
        }
        cx.notify();
    }

    /// The worker could not install the hooks: put the offer back so it can be tried again.
    pub fn hooks_offer_failed(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        if let Some(w) = self.workers.get_mut(&worker) {
            w.hooks_offered = false;
        }
        cx.notify();
    }

    /// The agent pill in a terminal's header: a short line in the tone of its status, which the
    /// status mark beside it draws as an icon. While the agent waits on the human the pill is a
    /// button that brings the terminal up so the TUI's own prompt can be answered there. Slopty
    /// never answers for the human.
    pub(super) fn agent_badge(
        &self,
        tile: TileRef,
        session: SessionId,
        agent: &AgentEvent,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        // The pill's tone is its status mark's; busy states (thinking, a tool) share the
        // accent, and the label says which.
        let Some(status) = Status::of_agent(agent) else { return div().into_any_element() };
        let (label, color) = (agent_status_text(agent), status.tone(theme));
        let item = tile.item;
        // An agent waiting on the human is the one state worth a click: the badge itself
        // goes to it. No second "go" beside it — a click on the tile did the same.
        let waiting = matches!(
            agent.status,
            AgentStatus::Blocked(
                BlockReason::Permission { .. } | BlockReason::Question | BlockReason::Elicitation,
            )
        );
        let ui_size = theme.typography.small() * k;
        let pill = div()
            .id("agent")
            .debug_selector(move || format!("agent-{}", item.as_uuid()))
            .role(if waiting { Role::Button } else { Role::Status })
            .aria_label(SharedString::from(label.clone()))
            .flex()
            .items_center()
            .flex_none()
            .max_w(px(ui_size * 22.0))
            .overflow_hidden()
            .gap(px(theme.spacing.xs * k))
            .px(px(theme.spacing.sm * k))
            .py(px(theme.spacing.xxs * k))
            .rounded(px(theme.radii.xs * k))
            .bg(hsla_alpha(color, alpha::FAINT))
            .text_size(px(ui_size))
            .text_color(hsla(color))
            .child(
                div().overflow_hidden().child(
                    ChromeText::new(label, px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                ),
            );
        let pill = if waiting {
            tab_stop(
                pill.cursor_pointer().hover(move |el| el.bg(hsla_alpha(color, alpha::TINT))),
                theme.surfaces.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.reveal_session(session, cx)))
        } else {
            pill
        };
        div()
            .id("badge")
            .debug_selector(move || format!("badge-{}", item.as_uuid()))
            .flex()
            .flex_none()
            .items_center()
            .child(pill)
            .into_any_element()
    }

    /// The badge for a long shell command that ended unwatched: its status and how long it
    /// took, in the tone of its status mark (done or failed). A press focuses the tile (which
    /// clears it).
    pub(super) fn finished_badge(
        &self,
        tile: TileRef,
        session: SessionId,
        done: &Finished,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        let tone = match done.exit {
            Some(0) | None => Status::Done,
            Some(_) => Status::Failed,
        }
        .tone(theme);
        let label = done.label();
        let item = tile.item;
        let pill = div()
            .id("finished")
            .debug_selector(move || format!("finished-{}", item.as_uuid()))
            .role(Role::Button)
            .aria_label(label.clone())
            .flex_none()
            .overflow_hidden()
            .px(px(theme.spacing.sm * k))
            .py(px(theme.spacing.xxs * k))
            .rounded(px(theme.radii.xs * k))
            .bg(hsla_alpha(tone, alpha::FAINT))
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(tone))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT)))
            .child(
                ChromeText::new(label, px(theme.typography.small()), k)
                    .fill()
                    .zooming(chrome.zooming),
            );
        tab_stop(pill, theme.surfaces.accent)
            .on_click(cx.listener(move |this, _ev, _window, cx| this.reveal_session(session, cx)))
            .into_any_element()
    }
}
