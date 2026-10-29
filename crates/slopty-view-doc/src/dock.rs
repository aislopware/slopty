//! slopty: what a session says beside its terminal, read-only: the inspector's header reads it
//! (Slopty's own; not in zeron).
//!
//! The view model serves it on `slopty.WatchDock` ([`crate::calls::WATCH_DOCK`]) for one chat:
//! one [`ChatDock`] each time anything in it changes. It holds the turn's live line (the TUI's
//! spinner row), the permission mode and the context by category. Nothing in it asks the agent
//! anything: its TUI answers in its terminal.
//!
//! Times are milliseconds since the epoch; whatever ticks (elapsed time, a retry's countdown,
//! the thinking words) is the shell's to tick from the times here, with the helpers on each
//! shape so every surface words it alike.

use serde::{Deserialize, Serialize};

/// A chat's dock.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDock {
    /// The turn under way, while one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<ActivityLine>,
    /// The permission mode, as its chip says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeChip>,
    /// The context by category, once the agent counted it, else its totals alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextBreakdown>,
}

/// The turn under way, as the TUI's spinner row shows it: "Cogitating… (1m 5s · ↓ 2.3k tokens ·
/// thinking some more)".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityLine {
    /// When the turn began: its elapsed time runs from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// Output tokens the model has written in the turn so far, as the agent estimates them:
    /// "↓ {N} tokens" once above zero.
    pub tokens: u64,
    /// Since when the model has been thinking, while it is ([`ActivityLine::thinking_words`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_since: Option<i64>,
    /// How long its last burst of thinking took: "thought for {N}s", shown a moment after it
    /// ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_ms: Option<u64>,
    /// How hard the model thinks, as the agent names the level ("thinking with high effort").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// What it is doing, in a phrase, in place of the shell's own verb: the agent's summary of
    /// the turn, the hook it runs, else the task in progress (the todo marked in progress).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrase: Option<String>,
    /// The next task of the list after the one in progress: the dim "Next: {task}" under the
    /// line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// The request being tried again, while it is: the line becomes the retry's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryLine>,
    /// Since when the model's reply, while one streams, has said nothing new: from
    /// [`ActivityLine::STALL_MS`] the line reddens and, from [`ActivityLine::NO_ANSWER_MS`],
    /// says the API has not answered, as the TUI's spinner does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_since: Option<i64>,
}

impl ActivityLine {
    /// Silence after which the TUI's spinner starts to redden.
    pub const STALL_MS: u64 = 10_000;
    /// Silence at which it is fully red.
    pub const STALLED_MS: u64 = 20_000;
    /// Silence after which the line says the API has not answered.
    pub const NO_ANSWER_MS: u64 = 30_000;

    /// How far the line has reddened at `now`: 0 before [`Self::STALL_MS`] of silence, 1 from
    /// [`Self::STALLED_MS`].
    #[must_use]
    pub fn stall(&self, now: i64) -> f32 {
        let Some(quiet) =
            self.quiet_since.and_then(|since| u64::try_from(now.saturating_sub(since)).ok())
        else {
            return 0.0;
        };
        let span = Self::STALLED_MS.saturating_sub(Self::STALL_MS);
        let into = quiet.saturating_sub(Self::STALL_MS).min(span);
        // Hundredths of a second under 10 s: exact as f32.
        let hundredths = |ms: u64| f32::from(u16::try_from(ms / 10).unwrap_or(u16::MAX));
        hundredths(into) / hundredths(span).max(1.0)
    }

    /// The thinking words after `elapsed_ms` of thinking, as the TUI moves through them:
    /// "thinking", "still thinking" (10 s), "thinking more" (20 s), "thinking some more" (30 s),
    /// "deep in thought" (45 s).
    #[must_use]
    pub const fn thinking_words(elapsed_ms: u64) -> &'static str {
        match elapsed_ms / 1_000 {
            0..=9 => "thinking",
            10..=19 => "still thinking",
            20..=29 => "thinking more",
            30..=44 => "thinking some more",
            _ => "deep in thought",
        }
    }
}

/// A request the agent tries again, as the line under way says it: "API error · Retrying in 5s ·
/// attempt 2/10".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryLine {
    /// The attempt that comes next, from 1.
    pub attempt: u32,
    /// How many it makes before it gives up, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    /// When the next attempt goes, when it says: the countdown runs to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    /// What failed, in a line ("API error", "Overloaded", "Session limit reached"): the TUI's
    /// first words, from the third attempt the error itself.
    pub what: String,
    /// The status page to look at when it persists, from the third attempt of an overload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl RetryLine {
    /// The line as it reads with `in_ms` left before the next attempt: "API error · Retrying in
    /// 5s · attempt 2/10".
    #[must_use]
    pub fn words(&self, in_ms: Option<u64>) -> String {
        let mut out = self.what.clone();
        match in_ms {
            Some(ms) if ms >= 1_000 => {
                out.push_str(&format!(" · Retrying in {}", crate::work::duration_words(ms)));
            }
            _ => out.push_str(" · Retrying now"),
        }
        match self.max {
            Some(max) => out.push_str(&format!(" · attempt {}/{max}", self.attempt)),
            None => out.push_str(&format!(" · attempt {}", self.attempt)),
        }
        out
    }
}

/// A passing notification of the agent's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteLine {
    /// Its key: a later one of the same key took the place of any before.
    pub key: String,
    /// What it says.
    pub text: String,
    /// How it is drawn: [`NoteLine::INFO`], [`NoteLine::WARNING`], [`NoteLine::ERROR`].
    pub tone: String,
    /// The agent's own priority (`low`, `medium`, `high`, `immediate`): an immediate one may
    /// toast.
    pub priority: String,
    /// When it came.
    pub at: i64,
    /// When it stops showing, when the agent says; the shell hides it then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

impl NoteLine {
    /// Something gone wrong.
    pub const ERROR: &'static str = "error";
    /// Plain news.
    pub const INFO: &'static str = "info";
    /// Worth a look.
    pub const WARNING: &'static str = "warning";
}

/// The permission mode, as its chip says it: "Bypass permissions on", amber for bypass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeChip {
    /// The agent's own name for it.
    pub id: String,
    /// Its name for people ("Accept edits").
    pub label: String,
    /// What the chip says ("Accept edits on"); empty for the agent's default mode, whose chip
    /// says nothing.
    pub words: String,
    /// How loud it is: [`ModeChip::DANGER`] (nothing is asked), [`ModeChip::PLAN`],
    /// [`ModeChip::ACCEPT`], [`ModeChip::QUIET`].
    pub tone: String,
}

impl ModeChip {
    /// Edits go ahead without asking.
    pub const ACCEPT: &'static str = "accept";
    /// Nothing is asked: bypass permissions, don't ask.
    pub const DANGER: &'static str = "danger";
    /// It plans and does not act.
    pub const PLAN: &'static str = "plan";
    /// The recommended default (auto) or plain asking (manual): the chip says the mode's name
    /// quietly.
    pub const QUIET: &'static str = "quiet";
}

/// The context window: how full it is, by category when the agent counted them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextBreakdown {
    /// Tokens in it.
    pub tokens: u64,
    /// The window.
    pub window: u64,
    /// How full it is, 0 to 100.
    pub used_percent: u8,
    /// Each category, in the agent's order, room left last; empty until the agent counted them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<ContextSlice>,
    /// How full it gets before it compacts on its own, 0 to 100; `None` when it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_percent: Option<u8>,
    /// What the TUI warns of as it fills: "12% until auto-compact", "Context low (8% remaining)
    /// · Run /compact to compact & continue"; `None` while there is room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<String>,
    /// The warning is urgent: the context is nearly full with nothing to compact it on its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub urgent: bool,
    /// What the last turn that ended took, by kind of token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<TurnTokens>,
}

/// The tokens one turn took, by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokens {
    /// Read as input, the cache aside.
    pub input: u64,
    /// Written.
    pub output: u64,
    /// Read from the prompt cache.
    pub cache_read: u64,
    /// Written to the prompt cache.
    pub cache_write: u64,
}

/// One category of the context ("System prompt", "Messages", "Free space").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSlice {
    /// Its name, as the agent names it.
    pub name: String,
    /// Its tokens.
    pub tokens: u64,
    /// Its share of the window, in hundredths of a percent.
    pub share_bp: u32,
    /// It is room left rather than room used.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub free: bool,
    /// The agent's name for the colour it draws it in, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The thinking words move on as the TUI's do.
    #[test]
    fn thinking_moves_through_its_words() {
        assert_eq!(ActivityLine::thinking_words(0), "thinking");
        assert_eq!(ActivityLine::thinking_words(12_000), "still thinking");
        assert_eq!(ActivityLine::thinking_words(31_000), "thinking some more");
        assert_eq!(ActivityLine::thinking_words(90_000), "deep in thought");
    }

    /// A retry counts down to its next attempt, out of the most it makes when it says.
    #[test]
    fn a_retry_reads_its_countdown_and_attempt() {
        let retry =
            RetryLine { attempt: 2, max: Some(10), what: "API error".into(), ..RetryLine::default() };
        assert_eq!(retry.words(Some(5_000)), "API error · Retrying in 5s · attempt 2/10");
        assert_eq!(retry.words(None), "API error · Retrying now · attempt 2/10");
    }
}
