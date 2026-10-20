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

/// slopty: Escape stops the agent on its second press (Amp): the first arms it, and the working
/// line ends "Esc again to stop" in warning ink while it is armed, then lets it go.
pub(crate) const ESCAPE_ARMED: std::time::Duration = std::time::Duration::from_millis(1_500);
/// What a screen reader hears of the armed stop.
pub(crate) const ARMED_SPOKEN: &str = "Press Escape again to stop the agent";
/// How long the armed hint takes to go at the end of [`ESCAPE_ARMED`].
const ESCAPE_HINT_FADE: std::time::Duration = std::time::Duration::from_millis(300);

thread_local! {
    /// The chat a first Escape armed, and when.
    static ARMED: std::cell::RefCell<Option<(String, std::time::Instant)>> =
        const { std::cell::RefCell::new(None) };
}

/// slopty: Escape pressed over chat `chat_id`'s turn at `now`: whether it stops the agent, a
/// second press while the first still holds; else it arms the stop.
pub(crate) fn escape_stops(chat_id: &str, now: std::time::Instant) -> bool {
    ARMED.with_borrow_mut(|armed| {
        let second = armed.as_ref().is_some_and(|(chat, at)| {
            chat == chat_id && now.saturating_duration_since(*at) < ESCAPE_ARMED
        });
        *armed = if second { None } else { Some((chat_id.to_owned(), now)) };
        second
    })
}

/// slopty: how much of the armed hint chat `chat_id`'s working line shows at `now`, 0 to 1:
/// whole while it holds, fading out over its last 300 ms.
pub(crate) fn escape_hint(chat_id: &str, now: std::time::Instant) -> f32 {
    ARMED.with_borrow(|armed| match armed {
        Some((chat, at)) if chat == chat_id => {
            let left = ESCAPE_ARMED.saturating_sub(now.saturating_duration_since(*at));
            (left.as_secs_f32() / ESCAPE_HINT_FADE.as_secs_f32()).min(1.0)
        }
        _ => 0.0,
    })
}

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
    // slopty: a first Escape armed the stop: the line ends by saying a second one stops it.
    let armed = armed_hint(chat_id, view, cx);
    let spoken = spoken(&words, armed.is_some().then_some(ARMED_SPOKEN));
    let lead = if words.retrying { words.lead.clone() } else { format!("{}…", words.lead) };
    let figures = figures_element(&words, view, turn, reduced, crate::clock::now(cx), &theme);
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
                    .children(figures)
                    .children(armed.map(|(hint, shown)| armed_element(hint, shown, &theme))),
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

/// slopty: the faint figures after the line's words: its timer, which rolls its digits under the
/// key the transcript's own working line rolls them under (`render_working_trailer`), so the line
/// taking over from that one mid-turn carries on rolling; then its tokens and thinking (#21).
/// The line as a screen reader hears it: its words, its figures and `stop_hint`, then what it
/// says under it.
fn spoken(words: &ActivityWords, stop_hint: Option<&str>) -> String {
    let figures = words.figures.iter().map(String::as_str).chain(stop_hint).collect::<Vec<_>>().join(" · ");
    [Some(words.lead.clone()), Some(figures).filter(|f| !f.is_empty()), words.below.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// slopty: what the working line says while a first Escape has armed the stop: "Esc again to
/// stop".
fn stop_hint() -> String {
    format!("{} again to stop", crate::keys::stroke_text(&gpui::Modifiers::default(), "escape"))
}

/// slopty: the armed hint for chat `chat_id`'s working line and how much of it shows, while a
/// first Escape holds; frames keep coming until it has gone.
pub(crate) fn armed_hint(chat_id: &str, view: EntityId, cx: &mut App) -> Option<(String, f32)> {
    let shown = escape_hint(chat_id, crate::clock::now(cx));
    (shown > 0.0).then(|| {
        crate::motion::pulse_lease(view, cx);
        (stop_hint(), shown)
    })
}

/// slopty: the armed hint after the line's figures, in warning ink, fading as the stop lets go.
pub(crate) fn armed_element(hint: String, shown: f32, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .relative()
        .top(px(1.0))
        .text_color(theme.warning)
        .opacity(shown)
        .child(SharedString::from(hint))
}

fn figures_element(
    words: &ActivityWords,
    view: EntityId,
    turn: i64,
    reduced: bool,
    now: std::time::Instant,
    theme: &Theme,
) -> Option<gpui::Div> {
    let (timer, rest) = match words.figures.split_first() {
        Some((timer, rest)) if !words.retrying => (Some(timer), rest),
        _ => (None, words.figures.as_slice()),
    };
    if timer.is_none() && rest.is_empty() {
        return None;
    }
    let rest = rest.join(" · ");
    Some(
        div()
            .flex_none()
            .relative()
            .top(px(1.0))
            .flex()
            .flex_row()
            .text_color(theme.text_faint)
            .font_features(crate::typography::tabular())
            .children(timer.map(|timer| {
                crate::roll_text::roll_text(
                    format!("working-timer-{view}-{turn}"),
                    SharedString::from(timer.clone()),
                    reduced,
                    now,
                )
            }))
            .when(!rest.is_empty(), |el| {
                el.child(SharedString::from(if timer.is_some() { format!(" · {rest}") } else { rest }))
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::dock::RetryLine;

    /// slopty: the dock's line rolls its timer under the transcript's own line's key, so taking
    /// over from it mid-turn the digits go on rolling (#21).
    #[test]
    fn the_timer_rolls_on_from_the_transcripts_line() {
        let (view, turn) = (EntityId::from(7u64), 3);
        let key = format!("working-timer-{view}-{turn}");
        let start = std::time::Instant::now();
        // The transcript's line said 4s, at rest.
        let _ = crate::roll_text::roll_text(key.clone(), "4s".into(), false, start);
        let line = ActivityLine { started_at: Some(0), tokens: 1_200, ..ActivityLine::default() };
        let words = activity_words(&line, "Working", 0, 5_000);
        assert_eq!(words.figures, ["5s", "↓ 1.2k tokens"]);
        let later = start + std::time::Duration::from_secs(1);
        assert!(figures_element(&words, view, turn, false, later, &Theme::dark()).is_some());
        assert!(
            crate::roll_text::rolling(key, "4s".into(), false, later).is_some(),
            "the dock's line moved the same timer on to 5s"
        );
    }

    /// slopty: Escape stops the agent on its second press: the first arms it and the line says
    /// so, in what it shows and in what a screen reader hears, until it lets go; a press over
    /// another chat, or after the hold, arms again instead.
    #[test]
    fn a_second_escape_stops_and_the_line_says_so() {
        assert_eq!(stop_hint(), "Esc again to stop");
        let start = std::time::Instant::now();
        assert_eq!(escape_hint("a", start), 0.0);
        assert!(!escape_stops("a", start), "the first press arms");
        assert_eq!(escape_hint("a", start), 1.0);
        assert_eq!(escape_hint("b", start), 0.0, "only over the chat it armed");
        let fading = start + ESCAPE_ARMED - ESCAPE_HINT_FADE / 2;
        assert!((escape_hint("a", fading) - 0.5).abs() < 0.01, "{}", escape_hint("a", fading));
        assert!(escape_stops("a", start + std::time::Duration::from_millis(400)), "the second stops");
        assert_eq!(escape_hint("a", start), 0.0, "and the hint goes with the stop");
        assert!(!escape_stops("a", start), "the next press arms again");
        assert!(!escape_stops("a", start + ESCAPE_ARMED), "after the hold, a press arms again");
        assert!(!escape_stops("b", start + ESCAPE_ARMED), "over another chat it arms that one");
        let line = ActivityLine { started_at: Some(0), ..ActivityLine::default() };
        let words = activity_words(&line, "Working", 0, 5_000);
        assert_eq!(
            spoken(&words, Some(ARMED_SPOKEN)),
            "Working · 5s · Press Escape again to stop the agent"
        );
        assert_eq!(spoken(&words, None), "Working · 5s");
        let bare = ActivityWords { figures: Vec::new(), ..words };
        let view = EntityId::from(7u64);
        assert!(figures_element(&bare, view, 1, true, start, &Theme::dark()).is_none());
    }

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
