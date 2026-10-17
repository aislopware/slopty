//! slopty: drilling into a subagent (Slopty's own; zeron opens a subagent as a right-pane tab).
//!
//! A subagent lives in its parent's chat (`docs/decisions/ui.md`, "Subagents live in their
//! parent's chat"); its card's Open shows the subagent's run as the session view, for reading a
//! long one. A breadcrumb at the top, "parent › subagent", leads back, as ⌘[ and Escape do. A
//! subagent of one of its parent's calls runs inside the parent's agent and takes no messages,
//! so a slim bar stands in the composer's place: "Runs inside {parent} · Stop". An agent another
//! agent started through Slopty takes messages, and keeps its composer.

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};

use super::Shell;
use crate::settings::widgets::{ActionHint as _, Keys};
use crate::theme::Theme;

/// The slim bar in place of the composer.
const BAR_HEIGHT: f32 = 40.0;

/// The subagent the session view shows, and its parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Drill {
    pub child: String,
    pub child_title: SharedString,
    pub parent: String,
    pub parent_title: SharedString,
    /// A subagent of one of its parent's calls: it takes no messages.
    pub in_parent: bool,
    /// It is at work.
    pub running: bool,
}

fn title_of(chat: &slopty_view_proto::Chat, fallback: &str) -> SharedString {
    let title = chat
        .title
        .clone()
        .or_else(|| chat.last_message_preview.clone())
        .unwrap_or_else(|| fallback.to_owned());
    crate::transcript::single_line(&title).into()
}

impl Shell {
    /// The subagent the session view shows now, when it shows one.
    pub(super) fn subagent_drill(&self, cx: &gpui::App) -> Option<Drill> {
        let state = self.state.read(cx);
        let child = state.selected_chat_row().filter(|chat| chat.subagent)?;
        let parent_id = child.parent_chat_id.clone()?;
        let parent = state.chats.iter().find(|chat| chat.id == parent_id);
        let running = state.indicator_for(&child.id, chrono::Utc::now())
            == crate::state::Indicator::Working;
        Some(Drill {
            child: child.id.clone(),
            child_title: title_of(child, "Subagent"),
            parent: parent_id,
            parent_title: parent.map_or_else(|| "Parent".into(), |chat| title_of(chat, "Parent")),
            in_parent: child.in_parent,
            running,
        })
    }

    /// Back from a subagent to its parent's chat.
    pub(super) fn leave_drill(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(drill) = self.subagent_drill(cx) else {
            return false;
        };
        self.open_chat(drill.parent, cx);
        true
    }

    /// "parent › subagent" over the session view's top.
    pub(super) fn render_drill_breadcrumb(&self, drill: &Drill, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let parent = drill.parent.clone();
        div()
            .absolute()
            .top(px(Theme::TITLEBAR_HEIGHT))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("subagent-breadcrumb")
                    .role(gpui::Role::Navigation)
                    .aria_label(SharedString::from(format!(
                        "{} › {}",
                        drill.parent_title, drill.child_title
                    )))
                    .h(px(26.0))
                    .max_w(px(560.0))
                    .px(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(crate::popover::surface_bg(&theme))
                    .text_size(crate::typography::ui_rems(12.0))
                    .child(
                        div()
                            .id("subagent-breadcrumb-parent")
                            .role(gpui::Role::Link)
                            // slopty: one key per tooltip, the keymap's.
                            .action_hint(
                                SharedString::from(format!("Back to {}", drill.parent_title)),
                                Keys::Action(Box::new(super::NavigateBack)),
                                cx,
                            )
                            .min_w_0()
                            .max_w(px(260.0))
                            .truncate()
                            .cursor_pointer()
                            .text_color(theme.text_muted)
                            .hover(|s| s.text_color(theme.text))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_chat(parent.clone(), cx);
                            }))
                            .child(drill.parent_title.clone()),
                    )
                    .child(div().flex_none().text_color(theme.text_faint).child("›"))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(260.0))
                            .truncate()
                            .text_color(theme.text)
                            .child(drill.child_title.clone()),
                    ),
            )
            .into_any_element()
    }

    /// "Runs inside {parent} · Stop", in the composer's place for a subagent that takes no
    /// messages; Escape goes back to the parent.
    pub(super) fn render_drill_bar(
        &mut self,
        drill: &Drill,
        width: f32,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if std::mem::take(&mut self.drill_focus_pending) {
            window.focus(&self.drill_focus, cx);
        }
        let child = drill.child.clone();
        let stopping = self.composer.read(cx).is_interrupting(&drill.child);
        let stop = (drill.running && !stopping).then(|| {
            div()
                .id("subagent-bar-stop")
                .role(gpui::Role::Button)
                .aria_label("Stop subagent")
                .h(px(24.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(Theme::CONTROL_RADIUS))
                .cursor_pointer()
                .text_color(theme.text)
                .bg(crate::theme::ink(0.06))
                .hover(|s| s.bg(crate::theme::ink(0.1)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let child = child.clone();
                    this.composer
                        .update(cx, |composer, cx| composer.interrupt_chat(child, cx));
                }))
                .child("Stop")
        });
        div()
            .id("subagent-bar")
            .track_focus(&self.drill_focus)
            .role(gpui::Role::Status)
            .aria_label(SharedString::from(format!("Runs inside {}", drill.parent_title)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.leave_drill(cx) {
                    cx.stop_propagation();
                }
            }))
            .debug_selector(|| "subagent-bar".into())
            .mx_auto()
            .mb(px(Theme::SPACE_MD))
            // slopty: the composer's own column: its pill stands inside its padding.
            .w(px((width - 2.0 * Theme::SPACE_LG).max(0.0)))
            .h(px(BAR_HEIGHT))
            .px(px(Theme::SPACE_MD))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(crate::popover::surface_bg(&theme))
            .text_size(crate::typography::ui_rems(13.0))
            .text_color(theme.text_muted)
            .child(
                crate::icons::icon(crate::icons::BOT)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .child(SharedString::from(format!("Runs inside {}", drill.parent_title))),
            )
            .children(stop)
            .when(stopping, |bar| {
                bar.child(div().flex_none().text_color(theme.text_faint).child("Stopping…"))
            })
            .into_any_element()
    }
}
