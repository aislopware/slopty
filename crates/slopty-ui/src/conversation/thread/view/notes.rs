//! The quiet lines between the steps: a compaction, which opens on the summary the thread
//! goes on from; a notice, with when the agent tries a failed request again; a review; an
//! item of a kind this client does not know.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_proto::thread::{Compaction, ItemBody, ItemId, Meters, Retry, Turn, Usage};

use super::{TOOL_ROW, ThreadView, composer, tokens};
use crate::colors::hsla;
use crate::conversation::figures::{model_name, spoken_model};
use crate::icons::IconName;
use crate::kit;

/// A compaction in words: "Compacted 120k → 18k tokens".
pub(super) fn compacted(c: &Compaction) -> String {
    match (c.before_tokens, c.after_tokens) {
        (Some(before), Some(after)) => {
            format!("Compacted {} \u{2192} {} tokens", tokens(before), tokens(after))
        }
        _ => "Compacted the conversation".to_owned(),
    }
}

/// When the agent tries again, the parts it says: "Retrying in 3 s · attempt 2 of 10".
pub(super) fn retrying(retry: &Retry) -> String {
    let when = retry.in_ms.map_or_else(
        || "Retrying".to_owned(),
        |ms| format!("Retrying in {}", kit::duration(Duration::from_millis(ms))),
    );
    let attempt = match retry.max {
        Some(max) => format!("attempt {} of {max}", retry.attempt),
        None => format!("attempt {}", retry.attempt),
    };
    format!("{when} \u{b7} {attempt}")
}

/// A settled turn's footer beyond its time: the model that answered, where it is not the
/// thread's own (a reroute, a switch), and what the turn spent, for its hint ("12k tokens ·
/// $0.0123").
pub(super) fn turn_footer(turn: &Turn, meters: &Meters) -> (Option<String>, Option<String>) {
    // The thread's own model, by every name the agent gives it: pi's `canned/canned-1` is
    // the turn's `canned-1`.
    let own = |m: &str| {
        [meters.model.as_deref(), meters.model_id.as_deref()].into_iter().flatten().any(|o| {
            [o.to_owned(), model_name(o), spoken_model(o).0]
                .iter()
                .any(|o| o.eq_ignore_ascii_case(m))
        })
    };
    let model = turn
        .models
        .iter()
        .rev()
        .filter(|m| !m.trim().is_empty())
        .map(|m| model_name(m))
        .find(|m| !own(m));
    let used = turn.usage.tokens();
    let cost = turn.usage.get(Usage::COST_MICRO_USD);
    let parts: Vec<String> =
        [(used > 0).then(|| format!("{} tokens", tokens(used))), (cost > 0).then(|| dollars(cost))]
            .into_iter()
            .flatten()
            .collect();
    (model, (!parts.is_empty()).then(|| parts.join(" \u{b7} ")))
}

/// The most lines a notice shows before "Show all".
const NOTICE_LINES: usize = 3;

/// A notice's first [`NOTICE_LINES`] lines, and whether more follow.
fn notice_lines(text: &str) -> (String, bool) {
    let mut lines = text.trim().lines();
    let shown: Vec<&str> = lines.by_ref().take(NOTICE_LINES).collect();
    (shown.join("\n"), lines.next().is_some())
}

/// Millionths of a dollar as a person reads a cost: "$0.0123", "$1.24".
pub(super) fn dollars(micro: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a cost on screen")]
    let usd = micro as f64 / 1_000_000.0;
    if usd < 1.0 { format!("${usd:.4}") } else { format!("${usd:.2}") }
}

impl ThreadView {
    pub(super) fn note_row(&self, ix: usize, id: &ItemId, cx: &Context<Self>) -> AnyElement {
        let Some(item) = self.item(ix, id, cx) else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let open = self.items_open.contains(id);
        let mut cut = false;
        let (icon, words, under, more) = match &item.body {
            ItemBody::Compaction(c) => {
                let summary = c.summary.as_ref().map(|t| t.text.clone()).filter(|t| !t.is_empty());
                (IconName::Scissors, compacted(c), None, summary)
            }
            ItemBody::Notice(n) => {
                let (shown, rest) = notice_lines(&n.text.text);
                cut = rest && !self.whole.contains(id);
                let words = if cut { shown } else { n.text.text.trim().to_owned() };
                (IconName::Info, words, n.retry.as_ref().map(retrying), None)
            }
            ItemBody::Review { .. } => {
                (IconName::ListChecks, "Reviewed the changes".to_owned(), None, None)
            }
            ItemBody::Extra { kind, .. } => (IconName::Info, composer::sentence(kind), None, None),
            _ => return div().into_any_element(),
        };
        let opens = more.is_some();
        let toggle = id.clone();
        let line = div()
            .id(ElementId::Name(format!("note-{}", id.0).into()))
            .debug_selector({
                let id = id.0.clone();
                move || format!("note-{id}")
            })
            .role(if opens { Role::Button } else { Role::Label })
            .aria_label(SharedString::from(words.clone()))
            .when(opens, |el| el.aria_expanded(open))
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .child(self.slot().child(self.icon(icon, s.text_muted)))
            .child(div().min_w_0().whitespace_normal().child(SharedString::from(words)))
            .when(opens, |el| {
                el.cursor_pointer()
                    .hover(move |el| el.text_color(hsla(s.text_secondary)))
                    .child(self.icon(
                        if open { IconName::ChevronDown } else { IconName::ChevronRight },
                        s.text_muted,
                    ))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.toggle_item(toggle.clone(), cx);
                    }))
            });
        let indent = self.z(TOOL_ROW + theme.spacing.xs);
        div()
            .w_full()
            .flex()
            .flex_col()
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(line)
            .children(under.map(|u| {
                let id = id.0.clone();
                kit::tabular(div())
                    .debug_selector(move || format!("retry-{id}"))
                    .pl(indent)
                    .text_size(self.z(theme.typography.small()))
                    .child(SharedString::from(u))
            }))
            .when(cut, |el| {
                let item = id.clone();
                el.child(
                    div()
                        .id(ElementId::Name(format!("note-all-{}", id.0).into()))
                        .debug_selector({
                            let id = id.0.clone();
                            move || format!("note-all-{id}")
                        })
                        .role(Role::Button)
                        .aria_label("Show all")
                        .pl(indent)
                        .cursor_pointer()
                        .hover(move |el| el.text_color(hsla(s.text)))
                        .child("Show all")
                        .on_click(
                            cx.listener(move |this, _ev, _w, cx| this.show_whole(item.clone(), cx)),
                        ),
                )
            })
            .children(more.filter(|_| open).map(|summary| {
                let id = id.0.clone();
                div()
                    .debug_selector(move || format!("summary-{id}"))
                    .pl(indent)
                    .pb(self.z(theme.spacing.xs))
                    .whitespace_normal()
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(summary))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{Compaction, Meters, Retry, Turn, TurnId, TurnState, Usage};

    use super::{compacted, dollars, notice_lines, retrying, turn_footer};

    /// A notice shows three lines and says when there is more; a short one shows whole.
    #[test]
    fn a_notice_shows_three_lines_and_says_when_there_is_more() {
        assert_eq!(notice_lines("one\ntwo"), ("one\ntwo".to_owned(), false));
        assert_eq!(notice_lines("a\nb\nc"), ("a\nb\nc".to_owned(), false));
        assert_eq!(notice_lines("a\nb\nc\nd\ne\n"), ("a\nb\nc".to_owned(), true));
    }

    /// A turn's footer names a model other than the thread's, and what the turn spent.
    #[test]
    fn a_turn_names_a_model_of_its_own_and_its_spend() {
        let mut usage = Usage::default();
        usage.0.insert(Usage::INPUT.to_owned(), 9_000);
        usage.0.insert(Usage::OUTPUT.to_owned(), 3_000);
        usage.0.insert(Usage::COST_MICRO_USD.to_owned(), 12_300);
        let turn = |models: &[&str]| Turn {
            id: TurnId(1),
            input: None,
            state: TurnState::Complete,
            started_ms: slopty_core::WallMs::ZERO,
            ended_ms: None,
            usage: usage.clone(),
            models: models.iter().map(|m| (*m).to_owned()).collect(),
            changed: slopty_proto::thread::Changed::default(),
            before: None,
            after: None,
        };
        let meters = Meters { model_id: Some("gpt-5.5".to_owned()), ..Meters::default() };
        assert_eq!(
            turn_footer(&turn(&["gpt-5.5-mini"]), &meters),
            (Some("gpt-5.5-mini".to_owned()), Some("12k tokens \u{b7} $0.0123".to_owned()))
        );
        assert_eq!(turn_footer(&turn(&["GPT-5.5"]), &meters).0, None, "the thread's own");
        let claude = Meters { model: Some("Opus 5.5".to_owned()), ..Meters::default() };
        assert_eq!(
            turn_footer(&turn(&["claude-haiku-4-5-20251001"]), &claude).0.as_deref(),
            Some("Haiku 4.5"),
            "named as a person says it"
        );
        assert_eq!(turn_footer(&turn(&["claude-opus-5-5"]), &claude).0, None);
        let pi = Meters {
            model: Some("Canned".to_owned()),
            model_id: Some("canned/canned-1".to_owned()),
            ..Meters::default()
        };
        assert_eq!(turn_footer(&turn(&["canned-1"]), &pi).0, None, "one model, said once");
        assert_eq!(dollars(1_240_000), "$1.24");
    }

    /// A retry says what the agent says of it, and nothing it does not.
    #[test]
    fn a_retry_says_when_and_which_attempt() {
        let retry = |max, in_ms| Retry { attempt: 2, max, in_ms };
        assert_eq!(
            retrying(&retry(Some(10), Some(3_000))),
            "Retrying in 3 s \u{b7} attempt 2 of 10"
        );
        assert_eq!(retrying(&retry(None, None)), "Retrying \u{b7} attempt 2");
    }

    /// A compaction reads as the context it left.
    #[test]
    fn a_compaction_reads_as_the_context_it_left() {
        let c = Compaction {
            trigger: None,
            before_tokens: Some(120_000),
            after_tokens: Some(18_000),
            summary: None,
        };
        assert_eq!(compacted(&c), "Compacted 120k \u{2192} 18k tokens");
    }
}
