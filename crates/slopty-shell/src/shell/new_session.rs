//! A new session, started at once (`docs/decisions/ui.md`, "A session is its
//! terminal"): an agent's own TUI, or a plain terminal, in one of the projects.
//!
//! The picker lists the projects, the one in context first, and the agent it starts above
//! them: ⇥ (or a press on its tab) picks the agent, ↩ starts it in the project at the cursor.
//! The view model starts it on the project's machine (`slopty.StartSession`); the session opens
//! as soon as the sidebar lists it, its terminal a moment after. A start its machine refuses
//! says why on its row. Nothing is said to the agent: the person types to it in its terminal.

use std::time::Duration;

use gpui::Focusable as _;
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Hsla, Pixels, SharedString, Subscription, Task,
    Window, div, px,
};
use slopty_view_doc::calls;

use super::Shell;
use super::command_palette::{
    command_key_hint, palette_card, palette_empty, palette_footer, palette_header,
    palette_overlay, palette_results_fade, palette_results_height,
};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::popover;
use crate::theme::Theme;

/// An agent a session can start: its id as the view model names it, and its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Agent {
    pub(super) id: &'static str,
    pub(super) name: &'static str,
}

/// The agents a session starts with, in the order the picker offers them; a plain terminal last.
pub(super) const AGENTS: [Agent; 4] = [
    Agent { id: "claude-code", name: "Claude Code" },
    Agent { id: "codex", name: "Codex" },
    Agent { id: "pi", name: "pi" },
    Agent { id: calls::TERMINAL_AGENT, name: "Terminal" },
];

/// The palette entry, and the picker's title.
pub(super) const NEW_SESSION: &str = "New session\u{2026}";

/// How long a started session is waited for in the sidebar's list.
const LISTED_WITHIN: Duration = Duration::from_secs(10);
/// How often it is looked for meanwhile.
const LISTED_POLL: Duration = Duration::from_millis(40);

/// An agent's mark, and the tint its brand wears.
pub(super) fn agent_mark(agent: Agent) -> (&'static str, Option<Hsla>) {
    match agent.id {
        "claude-code" => (icons::CLAUDE_MARK, Some(icons::claude_brand())),
        "codex" => (icons::OPENAI_MARK, None),
        "pi" => (icons::PI_MARK, None),
        _ => (icons::TERMINAL, None),
    }
}

/// What a new session starts as: an agent (or a shell) in a folder on a machine, and the title
/// the person gave it, if any.
pub(super) struct Start {
    pub device_id: String,
    pub cwd: String,
    pub agent: Agent,
    pub title: Option<String>,
}

/// The "New session…" picker.
pub(super) struct NewSessionPicker {
    search: Entity<ComposerInput>,
    focus: FocusHandle,
    focus_pending: bool,
    /// The agent it starts, of [`AGENTS`].
    agent: usize,
    /// The project row at the cursor.
    active: usize,
    /// The project in context, listed first.
    first: Option<String>,
    scroll: gpui::ScrollHandle,
    /// The start on its way: its project and the task.
    starting: Option<(String, Task<()>)>,
    /// The start that failed: its project and why.
    refused: Option<(String, String)>,
    _events: Subscription,
}

impl Shell {
    /// "New session…": the picker, the project in context first (`project`, else the open
    /// session's, else the sidebar's filter).
    pub(super) fn open_new_session_picker(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        self.command_palette = None;
        crate::live::chrome::release_held_keys(cx);
        let first = project.or_else(|| {
            let state = self.state.read(cx);
            state
                .selected_chat_row()
                .and_then(|chat| chat.space_id.clone())
                .or_else(|| state.selected_space.clone())
                .or_else(|| self.settings.space_filter.clone())
        });
        let search = cx.new(|cx| {
            ComposerInput::with_context("Search projects\u{2026}", "PaletteSearch", cx)
        });
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited)
                && let Some(picker) = this.new_session.as_mut()
            {
                picker.active = 0;
                picker.refused = None;
                cx.notify();
            }
        });
        self.new_session = Some(NewSessionPicker {
            search,
            focus: cx.focus_handle(),
            focus_pending: true,
            agent: 0,
            active: 0,
            first,
            scroll: gpui::ScrollHandle::new(),
            starting: None,
            refused: None,
            _events: events,
        });
        cx.notify();
    }

    /// Shut the picker, the keyboard back in the session's terminal.
    pub(super) fn close_new_session_picker(&mut self, cx: &mut Context<Self>) {
        if self.new_session.take().is_some() {
            self.focus_composer(cx);
            cx.notify();
        }
    }

    /// The projects the picker lists for its words: the one in context first, then the rest
    /// as the sidebar holds them.
    pub(super) fn new_session_rows(&self, cx: &gpui::App) -> Vec<slopty_view_proto::Space> {
        let Some(picker) = self.new_session.as_ref() else { return Vec::new() };
        let words = picker.search.read(cx).text().trim().to_lowercase();
        let state = self.state.read(cx);
        let machine = |device: &str| state.device_name(device).unwrap_or_default().to_lowercase();
        let mut rows: Vec<_> = state
            .spaces
            .iter()
            .filter(|space| {
                let said = format!(
                    "{} {} {}",
                    space.display_name().to_lowercase(),
                    space.path.to_lowercase(),
                    machine(&space.device_id)
                );
                words.split_whitespace().all(|word| said.contains(word))
            })
            .cloned()
            .collect();
        if let Some(first) = picker.first.as_deref()
            && let Some(ix) = rows.iter().position(|space| space.id == first)
        {
            let space = rows.remove(ix);
            rows.insert(0, space);
        }
        rows
    }

    fn move_new_session(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.new_session_rows(cx).len();
        let Some(picker) = self.new_session.as_mut() else { return };
        if count == 0 {
            return;
        }
        picker.active = if down { (picker.active + 1) % count } else { (picker.active + count - 1) % count };
        picker.scroll.scroll_to_item(picker.active);
        cx.notify();
    }

    fn step_new_session_agent(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(picker) = self.new_session.as_mut() else { return };
        let count = AGENTS.len();
        picker.agent = if forward { (picker.agent + 1) % count } else { (picker.agent + count - 1) % count };
        picker.refused = None;
        cx.notify();
    }

    fn start_picked_session(&mut self, cx: &mut Context<Self>) {
        let rows = self.new_session_rows(cx);
        let Some(picker) = self.new_session.as_ref() else { return };
        let Some(space) = rows.get(picker.active).cloned() else { return };
        let agent = AGENTS[picker.agent];
        self.start_session(space, agent, cx);
    }

    /// Start `agent` in project `space` on its machine; its session opens once listed.
    fn start_session(&mut self, space: slopty_view_proto::Space, agent: Agent, cx: &mut Context<Self>) {
        if self.new_session.as_ref().is_none_or(|picker| picker.starting.is_some()) {
            return;
        }
        let project = space.id.clone();
        let start = Start { device_id: space.device_id, cwd: space.path, agent, title: None };
        let refused_in = project.clone();
        let task = self.begin_session(
            start,
            |this, _| this.new_session = None,
            move |this, why, cx| this.new_session_refused(&refused_in, why, cx),
            cx,
        );
        if let (Some(picker), Some(task)) = (self.new_session.as_mut(), task) {
            picker.starting = Some((project, task));
            picker.refused = None;
        }
        cx.notify();
    }

    /// Start a session on its machine (`slopty.StartSession`). Once the sidebar lists it,
    /// `opened` runs (the picker that asked shuts), the session takes its `title` if one was
    /// given, and it opens; a start its machine refuses comes to `refused` in its words, at once
    /// with no engine to ask (and no task).
    pub(super) fn begin_session(
        &mut self,
        start: Start,
        opened: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        refused: impl FnOnce(&mut Self, String, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            refused(self, "Not connected to Slopty yet".to_owned(), cx);
            return None;
        };
        let params = serde_json::json!({
            "deviceId": start.device_id,
            "cwd": start.cwd,
            "agent": start.agent.id,
        });
        let title = start.title.filter(|title| !title.trim().is_empty());
        Some(cx.spawn(async move |this, cx| {
            let started = engine.client().call(calls::START_SESSION, params).await;
            let chat = match started {
                Ok(answer) => answer.get("chatId").and_then(|c| c.as_str()).map(str::to_owned),
                Err(error) => {
                    let _gone = this.update(cx, |this, cx| refused(this, error.to_string(), cx));
                    return;
                }
            };
            let Some(chat) = chat else {
                let why = "Its machine did not say the session".to_owned();
                let _gone = this.update(cx, |this, cx| refused(this, why, cx));
                return;
            };
            // The session opens once the sidebar's list has it.
            let since = cx.background_executor().now();
            loop {
                let listed = this
                    .update(cx, |this, cx| this.state.read(cx).chats.iter().any(|c| c.id == chat))
                    .unwrap_or(true);
                if listed || cx.background_executor().now().saturating_duration_since(since) >= LISTED_WITHIN {
                    break;
                }
                cx.background_executor().timer(LISTED_POLL).await;
            }
            let _gone = this.update(cx, |this, cx| {
                opened(this, cx);
                // The title is Slopty's, as a rename would set it: nothing is typed to the agent.
                if let Some(title) = title {
                    this.mutate(serde_json::json!({ "op": "renameChat", "chatId": chat, "title": title.trim() }), cx);
                }
                this.mark_starting(chat.clone(), cx);
                this.open_chat(chat, cx);
            });
        }))
    }

    fn new_session_refused(&mut self, project: &str, why: String, cx: &mut Context<Self>) {
        if let Some(picker) = self.new_session.as_mut() {
            picker.starting = None;
            picker.refused = Some((project.to_owned(), why));
        }
        cx.notify();
    }

    /// The picker over the window, while it is up.
    pub(super) fn render_new_session_picker(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        // A key reads as everywhere else (`keys::combo_text`).
        let shortcut =
            crate::keys::combo_text(self.settings.keymap.get(crate::settings::ShortcutId::NewSession));
        let rows = self.new_session_rows(cx);
        let machines: Vec<Option<String>> = {
            let state = self.state.read(cx);
            let many = state.devices.len() > 1;
            rows.iter()
                .map(|space| many.then(|| state.device_name(&space.device_id).map(str::to_owned)).flatten())
                .collect()
        };
        let picker = self.new_session.as_mut()?;
        if std::mem::take(&mut picker.focus_pending) {
            window.focus(&picker.search.focus_handle(cx), cx);
        }
        let search = picker.search.clone();
        let focus = picker.focus.clone();
        let scroll = picker.scroll.clone();
        let active = picker.active.min(rows.len().saturating_sub(1));
        let agent = AGENTS[picker.agent];
        let picked = picker.agent;
        let starting = picker.starting.as_ref().map(|(project, _)| project.clone());
        let refused = picker.refused.clone();
        let (mark, tint) = agent_mark(agent);
        let mut children: Vec<AnyElement> = Vec::new();
        for (ix, space) in rows.iter().enumerate() {
            let opening = starting.as_deref() == Some(space.id.as_str());
            let why = refused.as_ref().filter(|(project, _)| *project == space.id).map(|(_, why)| why.clone());
            let name: SharedString = space.display_name().to_owned().into();
            let place = match &machines[ix] {
                Some(machine) => format!("{machine} \u{00B7} {}", space.path),
                None => space.path.clone(),
            };
            let corner: Option<SharedString> = opening.then(|| "Starting\u{2026}".into());
            let target = space.clone();
            let row = popover::menu_row(&theme, ix == active, format!("new-session-row-{ix}"))
                .id(("new-session-row", ix))
                .rounded(px(popover::PALETTE_ITEM_RADIUS))
                .role(gpui::Role::Button)
                .aria_label(SharedString::from(format!("Start {} in {name}", agent.name)))
                .min_h(px(44.0))
                .py(px(5.0))
                .child(icon(mark).size(px(16.0)).flex_none().text_color(tint.unwrap_or(theme.text_muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().truncate().child(name))
                        .child(match why {
                            Some(why) => div()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.danger)
                                .child(SharedString::from(why)),
                            None => div()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(place)),
                        }),
                )
                .children(corner.map(|corner| {
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(corner)
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.start_session(target.clone(), agent, cx)))
                .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                    if let Some(picker) = this.new_session.as_mut()
                        && picker.active != ix
                    {
                        picker.active = ix;
                        cx.notify();
                    }
                }));
            children.push(div().px(px(8.0)).child(row).into_any_element());
        }
        let empty = rows.is_empty().then(|| {
            let words = search.read(cx).text().trim().is_empty();
            let (title, hint) = if words {
                ("No projects yet", "Add a project in the sidebar to start a session in it.")
            } else {
                ("No project matches", "Try another name, folder or machine.")
            };
            palette_empty(&theme, title, hint).into_any_element()
        });
        let body = div()
            .id("new-session-results")
            .min_h_0()
            .max_h(px(palette_results_height(viewport)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(super::SIDEBAR_LIST_GAP))
            .pt(px(8.0))
            .pb(px(8.0))
            .children(children)
            .children(empty);
        let body = palette_results_fade(body, &scroll);
        let mut tabs = div()
            .id("new-session-agents")
            .flex_none()
            .px(px(12.0))
            .py(px(6.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.06))
            .role(gpui::Role::TabList)
            .aria_label("Agent");
        for (ix, agent) in AGENTS.into_iter().enumerate() {
            let (mark, tint) = agent_mark(agent);
            let selected = ix == picked;
            tabs = tabs.child(
                crate::surface_chrome::tab(("new-session-agent", ix), selected, &theme)
                    .px(px(8.0))
                    .gap(px(5.0))
                    .role(gpui::Role::Tab)
                    .aria_label(agent.name)
                    .tooltip(crate::settings::widgets::text_tooltip(format!("Start {} \u{00B7} \u{21E5}", agent.name)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(picker) = this.new_session.as_mut() {
                            picker.agent = ix;
                            picker.refused = None;
                            cx.notify();
                        }
                    }))
                    .child(icon(mark).size(px(13.0)).text_color(match (selected, tint) {
                        (true, Some(tint)) => tint,
                        _ => theme.text_muted,
                    }))
                    .child(agent.name),
            );
        }
        // The keys, where there are keys: a phone or a pad has none to hint.
        let keys = (!crate::shell::touch_screen()).then(|| {
            palette_footer()
                .child(command_key_hint(&theme, "\u{2191} \u{2193}", "Navigate", window, cx))
                .child(command_key_hint(&theme, "\u{23CE}", "Start", window, cx))
                .child(command_key_hint(&theme, "Tab", "Agent", window, cx))
                .child(command_key_hint(&theme, "Esc", "Close", window, cx))
        });
        let card = palette_card("new-session-picker", &focus, viewport, &theme)
            .key_context("NewSessionPicker")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "up" | "down" => this.move_new_session(event.keystroke.key == "down", cx),
                    "enter" => this.start_picked_session(cx),
                    "tab" => this.step_new_session_agent(!event.keystroke.modifiers.shift, cx),
                    "escape" => this.close_new_session_picker(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_new_session_picker(cx)))
            .child(palette_header(
                &theme,
                search.into_any_element(),
                popover::kbd_hint(&theme, &shortcut),
            ))
            .child(tabs)
            .child(body)
            .children(keys);
        Some(palette_overlay(viewport, card))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every agent the picker offers has a name and a mark of its own; Claude Code wears its
    /// brand's colour, and a plain terminal comes last.
    #[test]
    fn the_agents_are_offered_with_their_marks() {
        assert_eq!(AGENTS[0].id, "claude-code");
        assert_eq!(AGENTS[AGENTS.len() - 1].id, calls::TERMINAL_AGENT);
        assert!(agent_mark(AGENTS[0]).1.is_some());
        let marks: std::collections::HashSet<_> = AGENTS.iter().map(|a| agent_mark(*a).0).collect();
        assert_eq!(marks.len(), AGENTS.len());
    }
}
