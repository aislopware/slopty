//! A turn's root row (`docs/decisions/inspector.md`, "The turn is the root row"). The
//! prompt that opened the turn is its row, and one line under the prompt says the turn's figures:
//! how it ended when it did not end well, the models that answered, the tokens it took (read
//! fresh, from the cache, and written; every kind in the tooltip), the lines it changed, and its
//! time, at the trailing edge where every step's time stands, ticking while it runs.

use gpui::prelude::*;
use gpui::{AnyElement, SharedString, div, px};
use slopty_view_doc::{TurnFacts, ran_for_words, tokens_short};

use super::subagents::model_words;
use super::{Transcript, format_full_timestamp, ticking_elapsed};
use crate::theme::Theme;

/// The height of the line of figures under a prompt: always drawn under a session's prompt, so
/// a turn's figures arriving move nothing.
pub(crate) const TURN_LINE: f32 = 20.0;

/// What a turn's line says.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TurnWords {
    /// How it ended when it did not end well ("Interrupted", "Failed · overloaded"), and whether
    /// it failed.
    pub fault: Option<(String, bool)>,
    /// Its models and tokens: "Opus 4.7 · 12k in · 340k cached · 2.1k out".
    pub figures: String,
    /// The lines it changed, when it changed any.
    pub changed: Option<[u32; 2]>,
    /// Its time: "1m 5s" once it ended, "1m 05s" ticking while it runs.
    pub time: String,
    /// Its tooltip: when it ran and its tokens by every kind.
    pub tip: String,
}

/// A count of tokens of `kind` as its tooltip names it.
fn kind_words(kind: &str) -> String {
    match kind {
        "input" => "Input".to_owned(),
        "output" => "Output".to_owned(),
        "cache-read" => "Cache read".to_owned(),
        "cache-write" => "Cache write".to_owned(),
        "reasoning" => "Reasoning".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|c| c.to_ascii_uppercase())
                .into_iter()
                .chain(chars)
                .collect::<String>()
                .replace('-', " ")
        }
    }
}

/// What turn `facts`'s line says at `now`, its times of day in `tz`.
pub(crate) fn turn_words<Tz: chrono::TimeZone>(facts: &TurnFacts, now: i64, tz: &Tz) -> TurnWords
where
    Tz::Offset: std::fmt::Display,
{
    let fault = match facts.state.as_str() {
        TurnFacts::INTERRUPTED => Some(("Interrupted".to_owned(), false)),
        TurnFacts::FAILED => Some((
            match facts.error.as_deref().map(super::single_line).filter(|error| !error.is_empty()) {
                Some(error) => format!("Failed · {error}"),
                None => "Failed".to_owned(),
            },
            true,
        )),
        _ => None,
    };
    let mut said: Vec<String> = Vec::new();
    for model in &facts.models {
        let words = model_words(model);
        if !said.contains(&words) {
            said.push(words);
        }
    }
    let [fresh, cached, out] = facts.tokens_in_cached_out();
    for (count, name) in [(fresh, "in"), (cached, "cached"), (out, "out")] {
        if count > 0 {
            said.push(format!("{} {name}", tokens_short(count)));
        }
    }
    let ran = facts.ran_ms(now);
    let time = if facts.ended.is_some() {
        ran_for_words(ran)
    } else {
        ticking_elapsed(i64::try_from(ran / 1_000).unwrap_or(i64::MAX))
    };
    let mut tip = vec![format!("Started {}", format_full_timestamp(facts.started, tz))];
    if let Some(ended) = facts.ended {
        tip.push(format!("Ended {}", format_full_timestamp(ended, tz)));
    }
    let kinds: Vec<String> = facts
        .tokens
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(kind, count)| format!("{} {}", kind_words(kind), tokens_short(*count)))
        .collect();
    if !kinds.is_empty() {
        tip.push(kinds.join(" · "));
    }
    TurnWords {
        fault,
        figures: said.join(" · "),
        changed: (facts.changed != [0, 0]).then_some(facts.changed),
        time,
        tip: tip.join("\n"),
    }
}

impl Transcript {
    /// The line of figures under prompt row `row_id` of turn `facts` (none yet: an empty line of
    /// the same height).
    pub(super) fn render_turn_line(
        &mut self,
        row_id: &SharedString,
        facts: Option<&TurnFacts>,
        theme: &Theme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let line = div()
            .id(SharedString::from(format!("{row_id}-turn")))
            .h(px(TURN_LINE))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(6.0))
            .whitespace_nowrap()
            .text_size(crate::typography::ui_rems(11.0))
            .font_features(crate::typography::tabular())
            .text_color(theme.text_faint);
        let Some(facts) = facts else {
            return line.into_any_element();
        };
        if facts.ended.is_none() {
            self.work.beat = Some(facts.started);
            self.tick_soon(cx);
        }
        let words = turn_words(facts, self.clock.timestamp_millis(), &chrono::Local);
        let spoken = [
            words.fault.as_ref().map(|(fault, _)| fault.clone()),
            (!words.figures.is_empty()).then(|| words.figures.clone()),
            words.changed.map(|[added, removed]| format!("+{added} −{removed}")),
            Some(words.time.clone()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let tip = words.tip.clone();
        line.role(gpui::Role::Status)
            .aria_label(SharedString::from(spoken))
            .tooltip(move |window, cx| {
                crate::settings::widgets::text_tooltip(tip.clone())(window, cx)
            })
            .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
            .children(words.fault.map(|(fault, failed)| {
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .truncate()
                    .text_color(if failed { theme.danger } else { theme.warning })
                    .child(SharedString::from(fault))
            }))
            .when(!words.figures.is_empty(), |line| {
                line.child(
                    div()
                        .min_w_0()
                        .flex_shrink(1.0)
                        .truncate()
                        .child(SharedString::from(words.figures)),
                )
            })
            .children(words.changed.map(|[added, removed]| {
                div()
                    .flex_none()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_color(theme.success)
                            .child(SharedString::from(format!("+{added}"))),
                    )
                    .child(
                        div()
                            .text_color(theme.danger)
                            .child(SharedString::from(format!("−{removed}"))),
                    )
            }))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(words.time)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn facts(state: &str, ended: Option<i64>) -> TurnFacts {
        TurnFacts {
            state: state.to_owned(),
            error: None,
            started: 1_000_000,
            ended,
            models: vec![
                "claude-opus-4-7".to_owned(),
                "claude-haiku-5-5".to_owned(),
                "claude-opus-4-7".to_owned(),
            ],
            tokens: BTreeMap::from([
                ("input".to_owned(), 1_200),
                ("cache-write".to_owned(), 800),
                ("cache-read".to_owned(), 340_000),
                ("output".to_owned(), 2_100),
            ]),
            changed: [12, 4],
        }
    }

    /// A turn that answered says its models once each, its tokens read fresh, from the cache
    /// and written, its lines and its time; nothing of how it ended.
    #[test]
    fn a_finished_turn_says_its_figures() {
        let words =
            turn_words(&facts(TurnFacts::COMPLETE, Some(1_065_000)), 2_000_000, &chrono::Utc);
        assert_eq!(words.fault, None);
        assert_eq!(words.figures, "Opus 4.7 · Haiku 5.5 · 2k in · 340k cached · 2.1k out");
        assert_eq!(words.changed, Some([12, 4]));
        assert_eq!(words.time, "1m 5s");
        assert!(words.tip.contains("Cache read 340k"), "{}", words.tip);
        assert!(words.tip.contains("Cache write 800"), "{}", words.tip);
    }

    /// A running turn's time ticks from its start; one that failed or was stopped says so first.
    #[test]
    fn a_turn_says_how_it_ended_only_when_not_well() {
        let running = turn_words(&facts(TurnFacts::ACTIVE, None), 1_065_000, &chrono::Utc);
        assert_eq!((running.fault, running.time.as_str()), (None, "1m 05s"));
        let stopped = turn_words(&facts(TurnFacts::INTERRUPTED, Some(1_002_000)), 0, &chrono::Utc);
        assert_eq!(stopped.fault, Some(("Interrupted".to_owned(), false)));
        let mut failed = facts(TurnFacts::FAILED, Some(1_002_000));
        failed.error = Some("Overloaded\nretry later".to_owned());
        let failed = turn_words(&failed, 0, &chrono::Utc);
        assert_eq!(failed.fault, Some(("Failed · Overloaded retry later".to_owned(), true)));
    }
}
