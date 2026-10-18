// slopty: Slopty's own file (not in zeron).
//! slopty: the turn's live line, as the TUI's spinner row says it: what the agent does (its own
//! summary, the hook it runs, the task in progress, else the transcript's step or flavour word),
//! then the elapsed time, "↓ N tokens", and the thinking words ("still thinking", "thinking with
//! high effort", "thought for 4s"); under it "Next: {task}". While a request is tried again the
//! line is the retry's, counting down to its next attempt: "API error · Retrying in 5s · attempt
//! 2/10", with the status page to look at under it once it persists.
//!
//! The transcript draws it in place of zeron's working line (its hook in `transcript.rs`,
//! `render_working_trailer`); the data is the chat's dock ([`super::dock::chat_dock`]).

use gpui::{AnyElement, App, EntityId, SharedString, div, prelude::*, px};
use slopty_view_doc::dock::ActivityLine;
use slopty_view_doc::work::{duration_words, tokens_short};

use crate::theme::Theme;

/// The live line's words at `now`.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityWords {
    /// What the agent does, or the retry's line.
    pub lead: String,
    /// It is a retry: drawn as a warning.
    pub retrying: bool,
    /// After it, faint: elapsed time, tokens, thinking.
    pub figures: Vec<String>,
    /// Under it: "Next: {task}", or the retry's hint.
    pub below: Option<String>,
    /// How far the reply's silence has warmed the line, 0 to 1 (the TUI's stall tint).
    pub stall: f32,
}

/// The words of `line` at `now`; `word` is what the transcript would say (the call under way or
/// its flavour word) when the agent says nothing of its own, `elapsed_secs` its timer.
pub fn activity_words(line: &ActivityLine, word: &str, elapsed_secs: i64, now: i64) -> ActivityWords {
    if let Some(retry) = &line.retry {
        let left = retry.at.map(|at| u64::try_from(at.saturating_sub(now)).unwrap_or(0));
        return ActivityWords {
            lead: retry.words(left),
            retrying: true,
            figures: Vec::new(),
            below: retry.hint.clone().map(|hint| format!("If it persists, check {hint}.")),
            stall: 0.0,
        };
    }
    let lead = line.phrase.clone().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| word.to_owned());
    let elapsed_ms = line
        .started_at
        .and_then(|at| u64::try_from(now.saturating_sub(at)).ok())
        .unwrap_or_else(|| u64::try_from(elapsed_secs.max(0)).unwrap_or(0) * 1_000);
    // slopty: the timer pads its seconds, so in tabular figures it holds its width.
    let mut figures = vec![crate::transcript::ticking_elapsed(
        i64::try_from(elapsed_ms / 1_000).unwrap_or(i64::MAX),
    )];
    if line.tokens > 0 {
        figures.push(format!("↓ {} tokens", tokens_short(line.tokens)));
    }
    if let Some(since) = line.thinking_since {
        let thinking = u64::try_from(now.saturating_sub(since)).unwrap_or(0);
        let words = ActivityLine::thinking_words(thinking);
        figures.push(match &line.effort {
            Some(effort) if words == "thinking" => format!("thinking with {effort} effort"),
            _ => words.to_owned(),
        });
    } else if let Some(ms) = line.thought_ms {
        figures.push(format!("thought for {}", duration_words(ms.max(1_000))));
    }
    // slopty: a reply silent past the TUI's patience says the API has not answered.
    let quiet = line.quiet_since.and_then(|since| u64::try_from(now.saturating_sub(since)).ok());
    let below = match quiet {
        Some(quiet) if quiet >= ActivityLine::NO_ANSWER_MS => Some(format!(
            "Waiting for API response · {} without a word · check your network",
            duration_words(quiet)
        )),
        _ => line.next.clone().filter(|n| !n.trim().is_empty()).map(|next| format!("Next: {next}")),
    };
    ActivityWords { lead, retrying: false, figures, below, stall: line.stall(now) }
}

/// The transcript's working line for chat `chat_id` drawn from its dock, or `None` while the
/// dock says no turn is under way (the transcript then draws its own). `turn` keys the rolling
/// words to the turn. `now` is the transcript's clock (ms since the epoch), taken on its tick and
/// not as the frame is built, so a frame built again says the second the shown one says.
pub fn working_line(
    chat_id: &str,
    word: &str,
    turn: i64,
    elapsed_secs: i64,
    now: i64,
    view: EntityId,
    cx: &mut App,
) -> Option<AnyElement> {
    let dock = super::dock::chat_dock(chat_id, cx)?;
    let line = dock.activity.as_ref()?;
    let words = activity_words(line, word, elapsed_secs, now);
    let theme = Theme::of(cx).clone();
    let reduced = cx.reduce_motion();
    let spoken = [Some(words.lead.clone()), Some(words.figures.join(" · ")).filter(|f| !f.is_empty()), words.below.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let lead = if words.retrying { words.lead.clone() } else { format!("{}…", words.lead) };
    let figures = words.figures.join(" · ");
    Some(
        div()
            .id("working-line")
            .role(gpui::Role::Status)
            .aria_label(SharedString::from(spoken))
            // slopty: the last row's lane (`transcript::ROW_LANE_HEIGHT`): its first line is the
            // strip's 24 px row, so the strip takes its place at the turn's end in the same room.
            .pt(px(Theme::SPACE_SM))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(Theme::SPACE_MD * 2.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(Theme::SPACE_SM))
                    .text_size(crate::typography::ui_rems(11.0))
                    .child(if words.retrying {
                        div().flex_none().size(px(6.0)).mx(px(3.0)).rounded_full().bg(theme.warning).into_any_element()
                    } else {
                        crate::loaders::gradient_spinner("working-indicator", &theme, 2.5, view, cx).into_any_element()
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            // slopty: a silent reply warms the words, as the TUI's spinner; a
                            // slow model is no failure, so amber at most.
                            .text_color(if words.retrying {
                                theme.warning
                            } else if words.stall > 0.0 {
                                crate::theme::mix(theme.text_muted, theme.warning, words.stall)
                            } else {
                                theme.text_muted
                            })
                            .child(crate::roll_text::roll_text(
                                format!("working-word-{view}-{turn}"),
                                SharedString::from(lead),
                                reduced,
                                crate::clock::now(cx),
                            )),
                    )
                    .when(!figures.is_empty(), |el| {
                        el.child(
                            div()
                                .flex_none()
                                .relative()
                                .top(px(1.0))
                                .text_color(theme.text_faint)
                                .font_features(crate::typography::tabular())
                                .child(SharedString::from(figures)),
                        )
                    }),
            )
            .when_some(words.below, |el, below| {
                el.child(
                    div()
                        .pl(px(Theme::SPACE_SM + 15.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .truncate()
                        .child(SharedString::from(below)),
                )
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::dock::RetryLine;

    /// The line names the agent's phrase, its time, its tokens and its thinking, the TUI's way.
    #[test]
    fn the_line_reads_phrase_time_tokens_and_thinking() {
        let line = ActivityLine {
            started_at: Some(0),
            tokens: 2_340,
            thinking_since: Some(40_000),
            effort: Some("high".into()),
            phrase: Some("Running full check".into()),
            next: Some("Write the docs".into()),
            ..ActivityLine::default()
        };
        let words = activity_words(&line, "Cogitating", 0, 65_000);
        assert_eq!(words.lead, "Running full check");
        assert_eq!(words.figures, ["1m 05s", "↓ 2.3k tokens", "thinking more"]);
        assert_eq!(words.below.as_deref(), Some("Next: Write the docs"));
        let early = activity_words(&line, "x", 0, 42_000);
        assert_eq!(early.figures.last().map(String::as_str), Some("thinking with high effort"));
        let quiet = ActivityLine { thought_ms: Some(4_200), ..ActivityLine::default() };
        let words = activity_words(&quiet, "Cogitating", 7, 0);
        assert_eq!(words.lead, "Cogitating");
        assert_eq!(words.figures, ["7s", "thought for 4s"]);
    }

    /// A reply silent past 10 s warms the line, fully at 20 s; past 30 s it says the API
    /// has not answered. A turn with no reply under way never does.
    #[test]
    fn a_silent_reply_warms_then_says_so() {
        let line = ActivityLine { quiet_since: Some(0), ..ActivityLine::default() };
        assert!(activity_words(&line, "x", 0, 9_000).stall.abs() < f32::EPSILON);
        let half = activity_words(&line, "x", 0, 15_000).stall;
        assert!((half - 0.5).abs() < 0.01, "{half}");
        let words = activity_words(&line, "x", 0, 31_000);
        assert!((words.stall - 1.0).abs() < f32::EPSILON);
        assert_eq!(
            words.below.as_deref(),
            Some("Waiting for API response · 31s without a word · check your network")
        );
        let tool_runs = ActivityLine::default();
        assert!(activity_words(&tool_runs, "x", 0, 99_000).stall.abs() < f32::EPSILON);
    }

    /// A retry takes the line over and counts down to its next attempt.
    #[test]
    fn a_retry_counts_down_in_the_line() {
        let line = ActivityLine {
            retry: Some(RetryLine {
                attempt: 3,
                max: Some(10),
                at: Some(5_000),
                what: "Overloaded".into(),
                hint: Some("status.claude.com".into()),
            }),
            ..ActivityLine::default()
        };
        let words = activity_words(&line, "x", 0, 0);
        assert!(words.retrying);
        assert_eq!(words.lead, "Overloaded · Retrying in 5s · attempt 3/10");
        assert_eq!(words.below.as_deref(), Some("If it persists, check status.claude.com."));
    }
}
