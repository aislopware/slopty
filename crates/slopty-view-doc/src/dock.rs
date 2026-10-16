//! slopty: what a chat's dock shows around its composer beside the queue and the tray (Slopty's
//! own; not in zeron).
//!
//! The view model serves it on `slopty.WatchDock` ([`crate::calls::WATCH_DOCK`]) for one chat:
//! one [`ChatDock`] each time anything in it changes. It holds the turn's live line (the TUI's
//! spinner row), the goal, the agent's passing notifications, the prompt it guesses comes next,
//! the person's own status line, the permission mode, the background counts, the context by
//! category, the subagent kinds `@` offers, the surfaces the agent's mods draw, and what the
//! chat's agent can be asked from the dock ([`DockCan`]).
//!
//! Times are milliseconds since the epoch; whatever ticks (elapsed time, a retry's countdown,
//! how long a goal has been active, the thinking words) is the shell's to tick from the times
//! here, with the helpers on each shape so every surface words it alike.

use serde::{Deserialize, Serialize};

use crate::work::WorkCounts;

/// A chat's dock.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDock {
    /// The turn under way, while one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<ActivityLine>,
    /// The goal the agent works towards, while one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalLine>,
    /// The agent's passing notifications still showing, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<NoteLine>,
    /// The prompt the agent guesses the person sends next: a quiet chip over the composer, which
    /// Tab takes into the draft. Never sent on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// The person's own status line, as its command printed it, in spans of one style; empty
    /// for none. A footer segment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_line: Vec<StatusSpan>,
    /// The permission mode, as its chip says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeChip>,
    /// The live background work by kind, as the footer counts it; a click opens the tray.
    pub background: WorkCounts,
    /// The background counts in words ("3 commands · 1 monitor"); empty with none.
    pub background_words: String,
    /// What the agent waits on now that its turn ended: "Waiting for 2 subagents to finish",
    /// "1 command still running"; the turn footer's line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    /// The context by category, once the agent counted it, else its totals alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextBreakdown>,
    /// The kinds of subagent the agent offers, which `@` lists after files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagent_kinds: Vec<SubagentKindRow>,
    /// What the agent's mods draw, in the order they came, toasts past their time left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<ModSurface>,
    /// What the dock may ask of the chat's agent.
    pub can: DockCan,
}

/// What a chat's agent can be asked from the dock; a control for a verb it cannot do is not
/// offered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockCan {
    /// Stop one piece of background work (`slopty.StopWork`).
    pub stop_work: bool,
    /// Move a running call to the background (`slopty.Background`).
    pub background: bool,
    /// Run a `!` command on the chat's machine (`slopty.RunShell`).
    pub shell: bool,
    /// Put the files back as they were before a message (`slopty.Rewind` with `code`).
    pub rewind: bool,
    /// Branch the conversation at a message (`slopty.Rewind` with `conversation`).
    pub fork: bool,
    /// Read and change its permission rules (`slopty.WatchRules`, `slopty.ChangeRule`).
    pub rules: bool,
    /// Act in its mods' surfaces (`slopty.SurfaceEvent`).
    pub surfaces: bool,
    /// Switch its permission mode (`slopty.SetMode`).
    pub set_mode: bool,
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

/// The goal the agent works towards: "◎ /goal active (5h)"; its condition shows on hover.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalLine {
    /// What has to hold for it to be met.
    pub condition: String,
    /// When it was set: how long it has been active runs from here.
    pub set_at: i64,
    /// The checks made so far ("{n} turns").
    pub iterations: u32,
    /// Tokens the session spent since it was set, as far as this client holds its turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// What the last check said, when one ran ("Last check: …").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
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

/// One run of the status line drawn in one style: its ANSI colours read into these.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSpan {
    /// Its text.
    pub text: String,
    /// Its colour: `ansi:{0-255}` for a palette colour, which the terminal theme maps, or
    /// `#rrggbb`; `None` for the footer's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    /// Its background, the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    /// Bold.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    /// Dim.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    /// Italic.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    /// Underlined.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
}

/// The permission mode, as its chip says it: "Bypass permissions on", amber for bypass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeChip {
    /// The agent's own name for it, as `slopty.SetMode` takes it.
    pub id: String,
    /// Its name for people ("Accept edits").
    pub label: String,
    /// What the chip says ("Accept edits on"); empty for the agent's default mode, whose chip
    /// says nothing.
    pub words: String,
    /// How loud it is: [`ModeChip::DANGER`] (nothing is asked), [`ModeChip::WARNING`] (a judge
    /// decides), [`ModeChip::PLAN`], [`ModeChip::ACCEPT`], [`ModeChip::QUIET`].
    pub tone: String,
    /// The modes it can be switched to, in the agent's order: what ⇧Tab cycles through.
    pub choices: Vec<ModeChoice>,
}

impl ModeChip {
    /// Edits go ahead without asking.
    pub const ACCEPT: &'static str = "accept";
    /// Nothing is asked: bypass permissions, don't ask.
    pub const DANGER: &'static str = "danger";
    /// It plans and does not act.
    pub const PLAN: &'static str = "plan";
    /// The agent's default: the chip says nothing.
    pub const QUIET: &'static str = "quiet";
    /// The agent's own judge of risk decides (auto mode).
    pub const WARNING: &'static str = "warning";
}

/// A mode the chip can switch to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeChoice {
    /// The agent's own name for it.
    pub id: String,
    /// Its name for people.
    pub label: String,
    /// What it does, when the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// The agent's MCP servers as its `/status` row counts them: "MCP servers: 3 connected, 1
    /// failed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servers: Option<String>,
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

/// A kind of subagent the agent offers, for `@`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentKindRow {
    /// Its name, as the agent calls it and `@` inserts it.
    pub name: String,
    /// What it is for.
    pub description: String,
    /// The model it runs on, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Something one of the agent's mods draws, in the shell's own components: a band over the
/// composer, a toast, a footer segment, a right-pane tab, a log line. No mod is special.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSurface {
    /// Its id, stable while it shows; `slopty.SurfaceEvent` names it.
    pub id: String,
    /// Where it shows: [`ModSurface::BAND`], [`ModSurface::TOAST`], [`ModSurface::STATUS`],
    /// [`ModSurface::PANE`], [`ModSurface::LOG`], else drawn as a band.
    pub kind: String,
    /// The mod that draws it.
    pub plugin: String,
    /// Its title, for a pane's tab or a band's heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its words, for a toast, a footer segment or a log line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Its element tree, nodes `{type, props, children}` of `Box`, `Text`, `Button`, `Input`,
    /// `Select`, `Link`, `Code`, `Markdown`, `Svg`; each one the person acts in has an `id`,
    /// which `slopty.SurfaceEvent`'s `element` names. `null` for a surface of words alone.
    #[serde(default)]
    pub tree: serde_json::Value,
    /// When a toast stops showing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

impl ModSurface {
    /// A band over the composer.
    pub const BAND: &'static str = "band";
    /// A line of the mod's log.
    pub const LOG: &'static str = "log";
    /// A tab of the right pane.
    pub const PANE: &'static str = "pane";
    /// A segment of the footer.
    pub const STATUS: &'static str = "status";
    /// A toast.
    pub const TOAST: &'static str = "toast";
}

/// What undoes a rewind (`slopty.Rewind`'s `undo`): the dock offers "Undo rewind" with it for
/// a few seconds, and `slopty.UndoRewind` takes it back: the code put back as it was, the
/// branched conversation deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindUndo {
    /// The chat rewound, shown again on undo.
    pub chat_id: String,
    /// The chat a conversation rewind branched, deleted on undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_chat_id: Option<String>,
    /// The folder before a code rewind, as a tree of its repository, put back on undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
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
