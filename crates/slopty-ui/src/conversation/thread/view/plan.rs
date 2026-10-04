//! A plan the agent proposes, as a document in the thread rather than a call: a card headed by
//! the map mark and its title alone at the base size (the mark says it is a plan, so no
//! label runs into the title), how it stands in the quiet tone and a copy, then its Markdown at
//! the prose size. A long plan shows its head until opened. While the agent waits on the
//! person's word, the card is edged in the warn tone and takes the answers, as a call's card
//! does.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, relative,
};
use slopty_proto::thread::{Clipped, ItemId, ToolCall, ToolState};
use slopty_theme::{Typography, alpha};

use super::ThreadView;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::IconName;
use crate::kit;

/// Lines of a long plan shown before it opens.
const HEAD: usize = 12;

/// A plan longer than this opens on request.
const FOLD: usize = 16;

/// How a plan stands, where the call says: only a plan put to the person was approved or not.
/// A plan an agent only states (Codex's) says nothing.
const fn standing(state: &ToolState) -> Option<&'static str> {
    match state {
        ToolState::Streaming => Some("Writing"),
        ToolState::Pending { .. } => Some("Awaiting approval"),
        ToolState::Failed | ToolState::Rejected => Some("Not approved"),
        ToolState::Cancelled => Some("Stopped"),
        ToolState::Running | ToolState::Completed => None,
    }
}

/// The lines of `body` shown: its head while it is long and closed. Whether more waits.
fn shown(body: &str, open: bool) -> (String, bool) {
    let lines = body.lines().count();
    if open || lines <= FOLD {
        return (body.to_owned(), false);
    }
    (body.lines().take(HEAD).collect::<Vec<_>>().join("\n"), true)
}

impl ThreadView {
    /// The card of plan `plan`, which call `id` proposed; `answers` are the person's, while the
    /// card asks.
    pub(super) fn plan_card(
        &self,
        id: &ItemId,
        call: &ToolCall,
        plan: &Clipped,
        answers: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (whole, clipped_more) = self.text_of(id, plan, cx);
        let theme = &self.theme;
        let s = theme.surfaces;
        let (title, body) = plan_parts(&whole);
        let lines = body.lines().count();
        let (body_shown, more) = shown(body, self.items_open.contains(id));
        let word = standing(&call.state);
        let copied = self.copied.as_ref() == Some(id);
        let copy = {
            let item = id.clone();
            let words = whole.clone();
            self.icon_button(
                format!("plan-copy-{}", id.0),
                if copied { IconName::Check } else { IconName::Copy },
                if copied { "Copied" } else { "Copy plan" },
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| {
                this.copy(item.clone(), words.clone(), cx);
            }))
        };
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(theme.density.row))
            .pl(self.z(theme.spacing.xs))
            .pr(self.z(theme.spacing.xxs))
            .child(self.slot().child(self.icon(IconName::Map, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.ui_size))
                    .text_color(hsla(s.text))
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .child(SharedString::from(title.clone())),
            )
            .children(word.map(|w| {
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(w)
            }))
            .child(copy);
        let open_more = more.then(|| {
            let item = id.clone();
            let label = SharedString::from(format!(
                "Show the whole plan \u{b7} {}",
                kit::count(lines as u64, "line", "lines")
            ));
            let selector = format!("plan-more-{}", id.0);
            crate::a11y::tab_stop(
                div()
                    .id(ElementId::Name(SharedString::from(selector.clone())))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .cursor_pointer()
                    .hover(move |el| el.text_color(hsla(s.text)))
                    .child(label),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle_item(item.clone(), cx)))
        });
        let clipped = (clipped_more && !more).then(|| self.show_all(id, plan, cx));
        let selector = format!("plan-{}", id.0);
        let label = match word {
            Some(word) => format!("Plan: {title}, {word}"),
            None => format!("Plan: {title}"),
        };
        kit::card(theme)
            .id(ElementId::Name(SharedString::from(selector.clone())))
            .debug_selector(move || selector)
            .role(Role::Article)
            .aria_label(SharedString::from(label))
            .w_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(self.z(theme.radii.md))
            .when(matches!(call.state, ToolState::Pending { .. }), |el| {
                el.border(kit::hair(theme)).border_color(hsla_alpha(s.warn, alpha::ASKING_EDGE))
            })
            .child(head)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xs))
                    .px(self.z(theme.spacing.md))
                    .pb(self.z(theme.spacing.sm))
                    .text_size(self.z(theme.typography.prose()))
                    .line_height(relative(theme.typography.prose_line_height))
                    .text_color(hsla(s.text))
                    .child(self.markdown(format!("plan-{}", id.0), &body_shown, false))
                    .children(open_more)
                    .children(clipped),
            )
            .children(answers)
            .into_any_element()
    }
}

/// A plan's title (its first heading, else its first line) and the Markdown under it.
#[must_use]
pub(crate) fn plan_parts(plan: &str) -> (String, &str) {
    let trimmed = plan.trim_start();
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    match first.trim().strip_prefix('#') {
        Some(heading) => (heading.trim_start_matches('#').trim().to_owned(), rest.trim_start()),
        None => ("Proposed plan".to_owned(), trimmed),
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AskId, ToolState};

    use super::{FOLD, HEAD, shown, standing};

    /// A plan put to the person says how it stands; one an agent only states says nothing.
    #[test]
    fn a_plan_says_how_it_stands_only_where_it_was_asked() {
        let asked = ToolState::Pending { ask: AskId("a".to_owned()) };
        assert_eq!(standing(&asked), Some("Awaiting approval"));
        assert_eq!(standing(&ToolState::Rejected), Some("Not approved"));
        assert_eq!(standing(&ToolState::Completed), None);
    }

    /// A long plan shows its head until opened; a short one shows whole.
    #[test]
    fn a_long_plan_shows_its_head_until_opened() {
        let long = (1..=FOLD + 1).map(|n| format!("{n}. step")).collect::<Vec<_>>().join("\n");
        let (head, more) = shown(&long, false);
        assert!(more);
        assert_eq!(head.lines().count(), HEAD);
        assert_eq!(shown(&long, true), (long.clone(), false));
        assert_eq!(shown("1. one", false), ("1. one".to_owned(), false));
    }
}
