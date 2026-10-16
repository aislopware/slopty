//! Slopty's own: an agent's subagents, each a row under its agent's row in the sidebar.
//!
//! A subagent is a chat another agent started ([`Chat::subagent`]), naming its agent's chat as
//! its parent. Those at work or waiting on the person always show; the rest fold behind one
//! "N finished" row per agent, closed until opened (for the run only), though the one open now
//! always shows. A click opens the subagent's transcript as the session view. The rows are the
//! sidebar's compact row stepped in under the agent's title, muted until hovered or open; they
//! ride inside their agent's slot, so pins, drags and jump slots see one row per session.

use chrono::{DateTime, Utc};
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use slopty_view_proto::{Chat, ChatIndicator};

use super::{SIDEBAR_LIST_GAP, Shell};
use crate::icons::{self, icon};
use crate::state::AppState;
use crate::theme::Theme;

/// A subagent row: the compact sidebar row's height.
const ROW_HEIGHT: f32 = 29.0;
/// How far a subagent row steps in: past its agent's status glyph and the gap after it, so the
/// subagent's own glyph sits under the agent's title.
const INDENT: f32 = 17.0;

/// One subagent as its row draws it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentRow {
    pub chat_id: String,
    pub title: SharedString,
    pub status: ChatIndicator,
    started: DateTime<Utc>,
    activity: DateTime<Utc>,
}

/// An agent's subagents, as its rows stand now.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Subagents {
    /// At work or waiting on the person, the longest at it first.
    pub working: Vec<SubagentRow>,
    /// The finished ones drawn under the fold: all of them while it is open, else only the one
    /// open now; the latest first.
    pub finished: Vec<SubagentRow>,
    /// How many have finished.
    pub finished_count: usize,
    /// The fold is open.
    pub open: bool,
}

impl Subagents {
    fn is_empty(&self) -> bool {
        self.working.is_empty() && self.finished_count == 0
    }

    /// The height the rows add under their agent's row, a gap above each.
    pub fn height(&self) -> f32 {
        let rows = self.working.len() + self.finished.len() + usize::from(self.finished_count > 0);
        rows as f32 * (ROW_HEIGHT + SIDEBAR_LIST_GAP)
    }
}

/// Whether a subagent is still at it: working, or waiting on the person.
fn at_work(status: ChatIndicator) -> bool {
    matches!(status, ChatIndicator::Working | ChatIndicator::AwaitingInput)
}

fn title(chat: &Chat) -> SharedString {
    let title = chat
        .title
        .clone()
        .or_else(|| chat.last_message_preview.clone())
        .unwrap_or_else(|| "Subagent".into());
    crate::transcript::single_line(&title).into()
}

/// The subagents of the chat `parent`, with the finished fold `open` or not.
pub(super) fn subagents(
    state: &AppState,
    parent: &str,
    open: bool,
    now: DateTime<Utc>,
) -> Subagents {
    let selected = state.selected_chat.as_deref();
    let (mut working, mut finished): (Vec<_>, Vec<_>) = state
        .chats
        .iter()
        .filter(|chat| {
            chat.subagent && !chat.archived && chat.parent_chat_id.as_deref() == Some(parent)
        })
        .map(|chat| SubagentRow {
            chat_id: chat.id.clone(),
            title: title(chat),
            status: state.display_status_for(chat, now),
            started: chat.created_at,
            activity: chat.last_message_at.unwrap_or(chat.created_at),
        })
        .partition(|row| at_work(row.status));
    // The longest at work leads, so one started long ago is not buried and new ones join at
    // the foot; the finished read latest first.
    working.sort_by_key(|row| row.started);
    finished.sort_by_key(|row| std::cmp::Reverse(row.activity));
    let finished_count = finished.len();
    if !open {
        finished.retain(|row| Some(row.chat_id.as_str()) == selected);
    }
    Subagents {
        working,
        finished,
        finished_count,
        open,
    }
}

impl Shell {
    /// The subagents of the chat `parent`, as the sidebar draws them under it.
    pub(super) fn sidebar_subagents(
        &self,
        parent: &str,
        now: DateTime<Utc>,
        cx: &gpui::App,
    ) -> Subagents {
        let open = self.sidebar_finished_subagents.contains(parent);
        subagents(self.state.read(cx), parent, open, now)
    }

    /// The agent's row `row` with its subagents' rows under it, in one column.
    pub(super) fn with_sidebar_subagents(
        &self,
        row: AnyElement,
        parent: &str,
        subagents: &Subagents,
        now: DateTime<Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if subagents.is_empty() {
            return row;
        }
        let selected = self.state.read(cx).selected_chat.clone();
        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_LIST_GAP))
            .child(row);
        for sub in &subagents.working {
            let open = selected.as_deref() == Some(sub.chat_id.as_str());
            column = column.child(self.render_sidebar_subagent(sub, open, now, theme, cx));
        }
        if subagents.finished_count > 0 {
            column = column.child(self.render_finished_subagents_fold(parent, subagents, theme, cx));
        }
        for sub in &subagents.finished {
            let open = selected.as_deref() == Some(sub.chat_id.as_str());
            column = column.child(self.render_sidebar_subagent(sub, open, now, theme, cx));
        }
        column.into_any_element()
    }

    fn render_sidebar_subagent(
        &self,
        row: &SubagentRow,
        selected: bool,
        now: DateTime<Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.chat_id.clone();
        let color = super::spaces::status_dot_color(row.status, theme);
        let glyph: AnyElement = match row.status {
            ChatIndicator::Working => crate::loaders::mini_glyph_spinner(
                format!("sidebar-subagent-{id}-working"),
                2.0,
                theme.glyph,
                self.sidebar_pane.entity_id(),
                cx,
            )
            .into_any_element(),
            ChatIndicator::Completed => icon(icons::CHECK)
                .size(px(11.0))
                .flex_none()
                .text_color(color)
                .into_any_element(),
            _ => div()
                .size(px(6.0))
                .flex_none()
                .rounded_full()
                .bg(color)
                .into_any_element(),
        };
        let (rest, hover) = if selected {
            let wash = crate::theme::glass_selected_bg();
            (wash, wash)
        } else {
            (crate::theme::wash(0.0), theme.glass_hover())
        };
        let text = theme.text;
        let time = slopty_view_proto::view::format_time_ago(row.activity, now);
        div()
            .id(SharedString::from(format!("sidebar-subagent-{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("sidebar-subagent-{id}")
            })
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Open subagent {}", row.title)))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .rounded(px(8.0))
            .pl(px(Theme::SPACE_SM + INDENT))
            .pr(px(Theme::SPACE_SM))
            .cursor_pointer()
            .bg(rest)
            .text_color(if selected { text } else { theme.text_muted })
            .hover(move |style| style.bg(hover).text_color(text))
            .on_click(cx.listener(move |this, _, _, cx| this.open_chat(id.clone(), cx)))
            .child(
                div()
                    .flex_none()
                    .size(px(13.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph),
            )
            .child(super::sidebar_faded_label(
                format!("sidebar-subagent-title-{}", row.chat_id).into(),
                true,
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .line_height(px(17.0))
                    .child(row.title.clone()),
            ))
            .child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted.opacity(0.5))
                    .child(time),
            )
            .into_any_element()
    }

    /// "N finished" under an agent's working subagents: opens and closes the fold.
    fn render_finished_subagents_fold(
        &self,
        parent: &str,
        subagents: &Subagents,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = subagents.open;
        let count = subagents.finished_count;
        let parent = parent.to_owned();
        let chevron = icon(icons::ALT_ARROW_RIGHT)
            .size(px(12.0))
            .text_color(theme.text_muted.opacity(0.5))
            .with_transformation(gpui::Transformation::rotate(gpui::percentage(if open {
                0.25
            } else {
                0.0
            })));
        let (text, hover) = (theme.text_muted, theme.glass_hover());
        div()
            .id(SharedString::from(format!("sidebar-subagents-finished-{parent}")))
            .debug_selector({
                let parent = parent.clone();
                move || format!("sidebar-subagents-finished-{parent}")
            })
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!(
                "{} {count} finished subagents",
                if open { "Hide" } else { "Show" }
            )))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .rounded(px(8.0))
            .pl(px(Theme::SPACE_SM + INDENT))
            .pr(px(Theme::SPACE_SM))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(text.opacity(0.7))
            .hover(move |style| style.bg(hover).text_color(text))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.sidebar_finished_subagents.remove(&parent) {
                    this.sidebar_finished_subagents.insert(parent.clone());
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex_none()
                    .size(px(13.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(chevron),
            )
            .child(SharedString::from(format!("{count} finished")))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, parent: Option<&str>, subagent: bool, minutes_ago: i64) -> Chat {
        let mut chat: Chat = serde_json::from_value(serde_json::json!({
            "id": id,
            "deviceId": "dev",
            "archived": false,
            "createdAt": Utc::now() - chrono::Duration::minutes(minutes_ago),
            "parentChatId": parent,
        }))
        .unwrap();
        chat.subagent = subagent;
        chat
    }

    /// Only an agent's subagents hang under it, not its side chats; those at work always show,
    /// finished ones fold until opened, except the one open now.
    #[test]
    fn subagents_hang_under_their_agent_and_finished_ones_fold() {
        let mut state = AppState::new();
        state.apply_chats(vec![
            chat("main", None, false, 60),
            chat("side", Some("main"), false, 5),
            chat("done-old", Some("main"), true, 40),
            chat("done-new", Some("main"), true, 10),
            chat("other", Some("elsewhere"), true, 3),
            chat("run-new", Some("main"), true, 2),
            chat("run-old", Some("main"), true, 20),
        ]);
        let now = Utc::now();
        let working = |id: &str| slopty_view_proto::Session {
            last_completed_turn: None,
            chat_id: id.into(),
            device_id: "dev".into(),
            status: slopty_view_proto::SessionStatus::Working,
            started_at: Some(now),
            updated_at: now,
            running_subagents: 0,
            status_since: None, // slopty: new field.
            waiting_on: None, // slopty: new field.
        };
        state.apply_sessions(vec![working("run-new"), working("run-old")]);
        let folded = subagents(&state, "main", false, now);
        let ids: Vec<_> = folded.working.iter().map(|row| row.chat_id.as_str()).collect();
        assert_eq!(ids, ["run-old", "run-new"], "at work always shows, the longest first");
        assert_eq!(folded.finished_count, 2);
        assert!(folded.finished.is_empty(), "folded");
        assert_eq!(folded.height(), 3.0 * (ROW_HEIGHT + SIDEBAR_LIST_GAP), "two at work, the fold");

        let open = subagents(&state, "main", true, now);
        let ids: Vec<_> = open.finished.iter().map(|row| row.chat_id.as_str()).collect();
        assert_eq!(ids, ["done-new", "done-old"], "latest first");

        state.selected_chat = Some("done-old".into());
        let folded = subagents(&state, "main", false, now);
        let ids: Vec<_> = folded.finished.iter().map(|row| row.chat_id.as_str()).collect();
        assert_eq!(ids, ["done-old"], "the open one shows though folded");
        assert!(subagents(&state, "side", false, now).is_empty());
    }
}
