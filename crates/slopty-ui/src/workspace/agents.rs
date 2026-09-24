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
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_proto::items::ItemKind;
use slopty_theme::alpha;

use super::actions::NextAttention;
use super::tile::Chrome;
use super::{Finished, WorkspaceEvent, WorkspaceView};
use crate::a11y::tab_stop;
use crate::chrome_text::ChromeText;
use crate::colors::{hsla, hsla_alpha};

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
        AgentStatus::Idle => "claude".to_owned(),
        AgentStatus::Working => detail.unwrap_or("working").to_owned(),
        AgentStatus::Tool { tool } => detail.unwrap_or(tool).to_owned(),
        AgentStatus::Blocked(BlockReason::Permission { tool }) => {
            format!("allow? {}", detail.unwrap_or(tool))
        }
        AgentStatus::Blocked(BlockReason::Question) => {
            format!("asking: {}", detail.unwrap_or("a question"))
        }
        AgentStatus::Blocked(BlockReason::Elicitation) => {
            format!("needs input: {}", detail.unwrap_or("an answer"))
        }
        AgentStatus::Blocked(BlockReason::IdlePrompt) => "idle".to_owned(),
        AgentStatus::Done => format!("done: {}", detail.unwrap_or("turn finished")),
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

    /// Sessions whose agent is waiting on the human, in reading order (workspace, column,
    /// tile) so ⌘⇧A walks the strip predictably.
    pub(super) fn needs_you(&self) -> Vec<(TileRef, SessionId)> {
        let mut out: Vec<(Option<slopty_client::layout::Pos>, TileRef, SessionId)> = self
            .items()
            .filter_map(|(worker, i)| match i.kind {
                ItemKind::Terminal { session } => Some((TileRef { worker, item: i.id }, session)),
                _ => None,
            })
            .filter(|(_, s)| self.agents.get(s).is_some_and(needs_human))
            .map(|(t, s)| (self.layout.position(t), t, s))
            .collect();
        out.sort_by_key(|(pos, t, _)| (pos.map(|p| (p.workspace, p.column, p.tile)), t.item));
        out.into_iter().map(|(_, t, s)| (t, s)).collect()
    }

    /// How many agents are waiting on the human, on every worker.
    #[must_use]
    pub fn needs_you_count(&self) -> usize {
        self.needs_you().len()
    }

    /// Agents waiting on the human on one worker.
    #[must_use]
    pub fn needs_you_on(&self, worker: WorkerKey) -> usize {
        self.needs_you().iter().filter(|(t, _)| t.worker == worker).count()
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
        let Some(&(_, first)) = waiting.first() else { return };
        let next = self
            .focused()
            .and_then(|focused| waiting.iter().position(|(t, _)| *t == focused))
            .and_then(|i| waiting.iter().cycle().nth(i.saturating_add(1)))
            .map_or(first, |(_, s)| *s);
        self.reveal_session(next, cx);
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

    /// The agent pill in a terminal's header: a coloured dot and a short line, plus a "go"
    /// button while the agent waits on the human, which brings the terminal up so the TUI's
    /// own prompt can be answered there. Slopty never answers for the human.
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
        let (label, color) = match &agent.status {
            AgentStatus::None => return div().into_any_element(),
            AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt) => {
                (agent_status_text(agent), theme.surfaces.text_muted)
            }
            // Busy states (thinking, a tool) share the accent: the label says which.
            AgentStatus::Working | AgentStatus::Tool { .. } => {
                (agent_status_text(agent), theme.surfaces.accent)
            }
            AgentStatus::Blocked(_) => (agent_status_text(agent), theme.surfaces.warn),
            AgentStatus::Done => (agent_status_text(agent), theme.surfaces.success),
        };
        let item = tile.item;
        let go = match &agent.status {
            AgentStatus::Blocked(
                BlockReason::Permission { .. } | BlockReason::Question | BlockReason::Elicitation,
            ) => {
                let tone = theme.surfaces.accent;
                let pad = if cfg!(target_os = "ios") { theme.spacing.md } else { theme.spacing.sm };
                let pill = div()
                    .id("go")
                    .debug_selector(move || format!("go-{}", item.as_uuid()))
                    .role(Role::Button)
                    .aria_label("go")
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(pad * k))
                    .py(px(theme.spacing.xxs * k))
                    .rounded(px(theme.radii.xs * k))
                    .bg(hsla_alpha(tone, alpha::TINT))
                    .text_size(px(theme.typography.small() * k))
                    .text_color(hsla(theme.surfaces.text))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla_alpha(tone, alpha::PRESSED)))
                    .child(
                        ChromeText::new("go", px(theme.typography.small()), k)
                            .zooming(chrome.zooming),
                    );
                Some(
                    tab_stop(pill, theme.surfaces.accent)
                        .on_click(
                            cx.listener(move |this, _ev, _w, cx| this.reveal_session(session, cx)),
                        )
                        .into_any_element(),
                )
            }
            _ => None,
        };
        let ui_size = theme.typography.small() * k;
        let pill = div()
            .id("agent")
            .debug_selector(move || format!("agent-{}", item.as_uuid()))
            .role(Role::Status)
            .aria_label(SharedString::from(label.clone()))
            .flex()
            .items_center()
            .flex_none()
            .max_w(px(ui_size * if go.is_some() { 14.0 } else { 22.0 }))
            .overflow_hidden()
            .gap(px(theme.spacing.xs * k))
            .px(px(theme.spacing.sm * k))
            .py(px(theme.spacing.xxs * k))
            .rounded(px(theme.radii.xs * k))
            .bg(hsla_alpha(color, alpha::FAINT))
            .text_size(px(ui_size))
            .text_color(hsla(color))
            .child(
                div()
                    .flex_none()
                    .size(px((theme.spacing.xs + theme.spacing.xxs) * k))
                    .rounded_full()
                    .bg(hsla(color)),
            )
            .child(
                div().overflow_hidden().child(
                    ChromeText::new(label, px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                ),
            );
        div()
            .id("badge")
            .debug_selector(move || format!("badge-{}", item.as_uuid()))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .child(pill)
            .children(go)
            .into_any_element()
    }

    /// The badge for a long shell command that ended unwatched: its status and how long it
    /// took, in the success or warn tone. A press focuses the tile (which clears it).
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
            Some(0) | None => theme.surfaces.success,
            Some(_) => theme.surfaces.warn,
        };
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
