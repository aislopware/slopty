// slopty: Slopty's own file (not in zeron).
//! slopty: side questions (`/btw`). A `/btw <question>` draft is never sent as a message: it
//! goes to the chat's agent as a side question (`slopty.AskSide`), answered from the
//! conversation so far, without tools, never entering it and never stopping the turn under way.
//! The newest exchange shows in a card above the composer, its question, then the agent at it,
//! its answer or why none came, until the person dismisses it; the next question brings it back.

use gpui::AnyElement;
use slopty_view_doc::SideLine;
use slopty_view_doc::calls;

use super::*;

/// The question a `/btw` draft asks, when the draft is one: its words after the command.
pub(crate) fn side_question(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix("/btw")?;
    if rest.starts_with(|c: char| !c.is_whitespace()) {
        return None;
    }
    let question = rest.trim();
    (!question.is_empty()).then_some(question)
}

impl Composer {
    /// Ask the chat's agent `question` beside the conversation; whether its agent takes one.
    pub(super) fn ask_side(&mut self, question: String, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self.dock.chat.clone() else { return false };
        if !self.dock.dock_of(&chat).is_some_and(|dock| dock.can.side) {
            return false;
        }
        self.dock.side_dismissed.remove(&chat);
        let params = serde_json::json!({ "question": question });
        self.dock_call(calls::ASK_SIDE, params, "Couldn't ask the side question", cx);
        true
    }

    /// The side exchange the card shows: the chat's newest, unless the person dismissed it.
    pub(crate) fn side_shown(&self, cx: &App) -> Option<SideLine> {
        let chat = self.state.read(cx).selected_chat.as_deref()?;
        let newest = self.dock.dock_of(chat)?.side.last()?;
        (self.dock.side_dismissed.get(chat) != Some(&newest.id)).then(|| newest.clone())
    }

    /// Put the card away until the next question.
    pub(crate) fn dismiss_side(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.state.read(cx).selected_chat.clone() else { return };
        let Some(id) = self.side_shown(cx).map(|side| side.id) else { return };
        self.dock.side_dismissed.insert(chat, id);
        cx.notify();
    }

    /// The card of the newest side exchange, above the composer.
    pub(super) fn render_side_card(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let side = self.side_shown(cx)?;
        let body = match (&side.answer, &side.failed) {
            (Some(answer), _) => {
                let tree = slopty_markdown::parse_full(answer);
                let opts = crate::markdown::render::RenderOptions::settled(SharedString::from(format!("side-{}", side.id)));
                div()
                    .id("dock-side-answer")
                    .max_h(px(220.0))
                    .overflow_y_scroll()
                    .text_size(crate::typography::ui_rems(13.0))
                    .child(crate::markdown::render::render_tree(&tree, &opts, theme, window, &|_| None))
                    .into_any_element()
            }
            (None, Some(why)) => div()
                .id("dock-side-failed")
                .role(Role::Status)
                .aria_label(SharedString::from(format!("No answer: {why}")))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.danger)
                .child(div().flex_none().size(px(5.0)).rounded_full().bg(theme.danger))
                .child(div().min_w_0().child(SharedString::from(why.clone())))
                .into_any_element(),
            (None, None) => div()
                .id("dock-side-pending")
                .role(Role::Status)
                .aria_label("Answering")
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_faint)
                .child(crate::loaders::mini_mono_spinner("dock-side-spinner", 1.5, theme.text_muted, cx.entity_id(), cx))
                .child("Answering beside the conversation")
                .into_any_element(),
        };
        let header = div()
            .h(px(28.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(11.5))
            .child(div().flex_none().font_family(theme.font_mono.clone()).text_color(theme.text_faint).child("/btw"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(side.question.clone())),
            )
            .child(
                div()
                    .id("dock-side-dismiss")
                    .role(Role::Button)
                    .aria_label("Dismiss the side answer")
                    .flex_none()
                    .size(px(22.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .bg(crate::motion::hover_blend("dock-side-dismiss", gpui::transparent_black(), crate::theme::wash(0.11)))
                    .on_hover(crate::motion::hover_listener("dock-side-dismiss"))
                    .tooltip(crate::settings::widgets::detail_tooltip(
                        "Dismiss",
                        "The next side question brings it back.",
                        None,
                    ))
                    .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_side(cx)))
                    .child(crate::icons::icon(crate::icons::CLOSE).size(px(10.0)).text_color(theme.text_muted)),
            );
        let card = crate::queue::queue_panel_surface(theme, false)
            .child(header)
            .child(div().px(px(8.0)).pb(px(8.0)).child(body));
        Some(
            div()
                .id("dock-side")
                .role(Role::Group)
                .aria_label(SharedString::from(format!("Side question: {}", side.question)))
                .mx(px(QUEUE_SIDE_INSET))
                .child(crate::frost::frosted(crate::queue::PANEL_RADIUS, crate::frost::MENU_BLUR, card))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `/btw` draft asks the words after the command; `/btwx`, an empty one and one further in
    /// ask nothing.
    #[test]
    fn a_btw_draft_names_its_question() {
        assert_eq!(side_question("/btw which file?"), Some("which file?"));
        assert_eq!(side_question("  /btw\n why ? "), Some("why ?"));
        assert_eq!(side_question("/btw"), None);
        assert_eq!(side_question("/btw   "), None);
        assert_eq!(side_question("/btwx why"), None);
        assert_eq!(side_question("say /btw why"), None);
    }
}
