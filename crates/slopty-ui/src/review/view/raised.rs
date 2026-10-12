//! What the agent's own review raised, Delta's way (`.research/delta-signed-in-2026-10-12.md`
//! §3.11): each finding on a line of the diff is a comment under it, and the comparison row
//! says what came, "Raised 2 findings ⌄", which opens a float with how the review came out in
//! full and the findings that name no line on show. Nothing stands over the diff: the change
//! under review keeps the tile.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_kit::colors::hsla;
use slopty_kit::icons::Symbol;
use slopty_kit::kit;
use slopty_theme::Rgb;

use super::ReviewView;
use crate::review::findings;
use crate::review::model::Note;

/// The most the float of the findings takes before it scrolls.
const FINDINGS_HEIGHT: f32 = 320.0;

/// How wide the float of the findings is at most: a notice's measure
/// (`workspace::toast::TOAST_MAX_W`).
const FINDINGS_WIDTH: f32 = 465.0;

impl ReviewView {
    /// What the comparison row says came of the agent's review, and in which tone: its words
    /// up to their reason, the agent's name left to the float ("Raised 2 findings", "The review
    /// didn't start"); the findings' count once that word is let go. `None` with nothing to say.
    fn raised_words(&self) -> Option<(String, Rgb)> {
        let s = self.theme.surfaces;
        let Some(came) = &self.came else {
            let n = self.model.notes().len();
            let n = u64::try_from(n).unwrap_or(u64::MAX);
            return (n > 0).then(|| (kit::count(n, "finding", "findings"), s.text_secondary));
        };
        let head = came.words.split_once(": ").map_or(came.words.as_str(), |(head, _)| head);
        let own = Some(came.agent.as_str())
            .filter(|agent| !agent.is_empty())
            .and_then(|agent| head.strip_prefix(agent))
            .map(str::trim_start);
        let words = own.map_or_else(|| head.to_owned(), capitalised);
        Some((words, if came.failed { s.error } else { s.text_secondary }))
    }

    /// The comparison row's word of what came, as [`Self::raised_words`] says it.
    #[cfg(test)]
    pub(crate) fn raised(&self) -> Option<String> {
        self.raised_words().map(|(words, _)| words)
    }

    /// The comparison row's word of what came, a ghost button with the agent's mark and a
    /// chevron, and its float while open.
    pub(super) fn raised_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (words, tone) = self.raised_words()?;
        // A screen reader hears what came in full, the agent's name and the reason too.
        let full = self.came.as_ref().map_or_else(|| words.clone(), |came| came.words.clone());
        let theme = &self.theme;
        let s = theme.surfaces;
        let open = self.findings_open;
        let button = div()
            .id("review-findings-toggle")
            .debug_selector(|| "review-findings-toggle".to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(full))
            .aria_expanded(open)
            .flex_none()
            .h(px(theme.density.button))
            .px(px(theme.spacing.xs + theme.spacing.xxs))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs))
            .whitespace_nowrap()
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .text_color(hsla(tone))
            .when(open, move |el| el.bg(hsla(s.selected)))
            .hover(move |el| el.bg(hsla(s.hover)))
            .child(self.icon(slopty_kit::icons::AGENT, tone))
            .child(SharedString::from(words))
            .child(
                slopty_kit::icons::Drawn::disclosure(theme, Symbol::ChevronDown)
                    .slot(px(theme.icon()), hsla(s.icon_muted)),
            )
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.findings_open = !this.findings_open;
                cx.notify();
            }));
        let float = open.then(|| {
            let notes =
                self.model.notes().iter().enumerate().map(|(ix, note)| self.note_row(ix, note, cx));
            let width = FINDINGS_WIDTH.min(theme.spacing.md.mul_add(-2.0, self.width)).max(0.0);
            let panel = kit::elevate(div(), theme)
                .id("review-findings")
                .debug_selector(|| "review-findings".to_owned())
                .role(Role::List)
                .aria_label("The agent's review")
                .occlude()
                .mt(px(theme.spacing.xxs))
                .w(px(width))
                .max_h(px(FINDINGS_HEIGHT))
                .overflow_y_scroll()
                .rounded(px(theme.radii.lg))
                .py(px(theme.spacing.xxs))
                .text_size(px(theme.typography.small()))
                .line_height(gpui::relative(theme.typography.markdown_line_height))
                .children(self.came_row(cx))
                .children(notes);
            gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopLeft).child(panel))
                .with_priority(slopty_kit::layer::Layer::Submenu.priority())
        });
        Some(
            div()
                .relative()
                .flex_none()
                .child(slopty_kit::a11y::tab_stop(button, s.focus))
                .children(float)
                .into_any_element(),
        )
    }

    /// How the agent's review came out, in full, with the way to let the word go.
    fn came_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        self.came.as_ref().map(|came| {
            let tone = if came.failed { s.error } else { s.text_secondary };
            div()
                .debug_selector(|| "review-came".to_owned())
                .w_full()
                .flex()
                .items_start()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xs))
                .child(
                    div()
                        .id("review-came-words")
                        .role(Role::Status)
                        .aria_label(SharedString::from(came.words.clone()))
                        .min_w_0()
                        .flex_1()
                        .whitespace_normal()
                        .text_color(hsla(tone))
                        .child(SharedString::from(came.words.clone())),
                )
                .child(self.close(
                    "review-came-close",
                    "Dismiss",
                    cx.listener(|this, _ev, _w, cx| this.dismiss_came(cx)),
                ))
                .into_any_element()
        })
    }

    /// A finding with no line on show: where it is, what it says, and the way to let it go.
    fn note_row(&self, ix: usize, note: &Note, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let finding = &note.finding;
        let id = format!("review-note-{ix}");
        let selector = id.clone();
        let label = format!(
            "{}. {}",
            finding.title,
            finding.place.as_ref().map(findings::Place::words).unwrap_or_default()
        );
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || selector)
            .role(Role::ListItem)
            .aria_label(SharedString::from(label))
            .w_full()
            .flex()
            .items_start()
            .gap(px(theme.spacing.xs))
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xs))
            .border_t(kit::HAIR)
            .border_color(hsla(s.stroke))
            .child(self.icon(Symbol::TextBubble, s.text_secondary))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(theme.spacing.xxs))
                    .children(finding.place.as_ref().map(|place| {
                        div()
                            .font_family(self.mono())
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(place.words()))
                    }))
                    .child(
                        div()
                            .whitespace_normal()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(finding.title.clone())),
                    )
                    .when(!finding.body.is_empty(), |el| {
                        el.child(
                            div()
                                .whitespace_normal()
                                .text_color(hsla(s.text_secondary))
                                .child(SharedString::from(finding.body.clone())),
                        )
                    }),
            )
            .child(self.close(
                format!("review-unnote-{ix}"),
                "Let the finding go",
                cx.listener(move |this, _ev, _w, cx| this.unnote(ix, cx)),
            ))
            .into_any_element()
    }
}

/// `words` with its first letter upper case: sentence case once the agent's name before it is
/// gone.
fn capitalised(words: &str) -> String {
    let mut letters = words.chars();
    letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect())
}

#[cfg(test)]
mod tests {
    use super::capitalised;

    #[test]
    fn the_first_letter_goes_up() {
        assert_eq!(capitalised("raised 2 findings"), "Raised 2 findings");
        assert_eq!(capitalised(""), "");
    }
}
