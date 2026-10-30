//! A chat's background work, as the inspector's list of what runs reads it.
//!
//! The view model serves it on `slopty.WatchWork` ([`crate::calls::WATCH_WORK`]) for one chat:
//! one [`WorkTray`] each time anything in it changes. It is the agent's own background tasks
//! (commands, monitors, Claude Code's subagents, workflows, MCP tasks) beside the child chats
//! another agent started under this one through Slopty, merged into one list, so a long run never
//! loses sight of what it left running (`docs/decisions/ui.md`, "Subagents live in their parent's
//! chat").
//!
//! The words a person reads ([`WorkRow::state_words`]) come worded, so every surface says them
//! alike.

use serde::{Deserialize, Serialize};

/// A chat's background work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTray {
    /// What is at work now, housekeeping of the agent's own (a memory pass) left out, in the
    /// order the inspector lists it: what needs the person first, then by when it began; a task
    /// another task started follows its parent.
    pub rows: Vec<WorkRow>,
}

/// One piece of background work at work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRow {
    /// Its id: `task:{id}` for one of the agent's own tasks, `chat:{chatId}` for a child chat
    /// no task stands for.
    pub id: String,
    /// What kind of work: [`WorkRow::COMMAND`], [`WorkRow::MONITOR`], [`WorkRow::SUBAGENT`],
    /// [`WorkRow::WORKFLOW`], [`WorkRow::MCP`], [`WorkRow::REMOTE`], [`WorkRow::TEAMMATE`], else
    /// the agent's own word.
    pub kind: String,
    /// The kind as a noun ("command", "subagent", "MCP task"), for assistive technologies.
    pub noun: String,
    /// What it is: the command, the monitor's description, the subagent's description.
    pub title: String,
    /// Where it stands in the inspector's words: "running", "waiting", "paused", "needs you".
    pub state_words: String,
    /// What it is doing now, in a phrase: a subagent's own progress summary, else its last tool
    /// ("Running full check").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The child chat its run is, where Slopty keeps one: what a press drills into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
}

impl WorkRow {
    /// A command left running in the background.
    pub const COMMAND: &'static str = "command";
    /// An MCP server's long task.
    pub const MCP: &'static str = "mcp";
    /// A watch that wakes the agent on each event.
    pub const MONITOR: &'static str = "monitor";
    /// An agent running elsewhere (a cloud session).
    pub const REMOTE: &'static str = "remote";
    /// A subagent: Claude Code's own, or an agent another started through Slopty.
    pub const SUBAGENT: &'static str = "subagent";
    /// A teammate of an agent team.
    pub const TEAMMATE: &'static str = "teammate";
    /// A dynamic workflow.
    pub const WORKFLOW: &'static str = "workflow";
}

/// A count of tokens as a person reads it: `950`, `14k`, `1.2M`.
#[must_use]
pub fn tokens_short(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = tokens.saturating_add(50_000) / 100_000;
        let (whole, part) = (tenths / 10, tenths % 10);
        if part == 0 { format!("{whole}M") } else { format!("{whole}.{part}M") }
    } else if tokens >= 1_000 {
        let tenths = tokens.saturating_add(50) / 100;
        let (whole, part) = (tenths / 10, tenths % 10);
        if part == 0 || whole >= 100 {
            format!("{}k", tokens.saturating_add(500) / 1_000)
        } else {
            format!("{whole}.{part}k")
        }
    } else {
        tokens.to_string()
    }
}

/// A size in bytes as Slopty says it everywhere, the unit after a no-break space so the two never
/// part at a line's end: `512 B`, `12.3 KB`, `1.4 MB`, `2.0 GB` (1024 a step).
#[must_use]
pub fn bytes_short(bytes: u64) -> String {
    const NBSP: char = '\u{a0}';
    let tenths = |unit: u64| bytes.saturating_mul(10).checked_div(unit).unwrap_or(0);
    let (unit, name) = match bytes {
        b if b >= 1 << 30 => (1 << 30, "GB"),
        b if b >= 1 << 20 => (1 << 20, "MB"),
        b if b >= 1 << 10 => (1 << 10, "KB"),
        _ => return format!("{bytes}{NBSP}B"),
    };
    let t = tenths(unit);
    format!("{}.{}{NBSP}{name}", t / 10, t % 10)
}

/// A span of time as the TUI says it, its two most significant units: `45s`, `2m 5s`,
/// `1h 51m`, `3d 4h`.
#[must_use]
pub fn duration_words(ms: u64) -> String {
    let secs = ms / 1_000;
    let (days, hours, minutes, seconds) =
        (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    // Two units at most, and a second unit of nothing is left out: "1m", never "1m 0s".
    let (big, unit, small, small_unit) = if days > 0 {
        (days, "d", hours, "h")
    } else if hours > 0 {
        (hours, "h", minutes, "m")
    } else if minutes > 0 {
        (minutes, "m", seconds, "s")
    } else {
        return format!("{seconds}s");
    };
    if small == 0 { format!("{big}{unit}") } else { format!("{big}{unit} {small}{small_unit}") }
}

/// How long a finished run ran, as the tray and its notice both say it: [`duration_words`],
/// and "<1s" for a run shorter than a second, never "0s".
#[must_use]
pub fn ran_for_words(ms: u64) -> String {
    if ms < 1_000 { "<1s".to_owned() } else { duration_words(ms) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run under a second ran for less than one, wherever it is said.
    #[test]
    fn a_short_run_reads_under_a_second() {
        assert_eq!(ran_for_words(300), "<1s");
        assert_eq!(ran_for_words(1_000), "1s");
        assert_eq!(ran_for_words(125_000), "2m 5s");
    }

    /// Tokens and times read as the TUI's do.
    #[test]
    fn figures_read_short() {
        assert_eq!(tokens_short(950), "950");
        assert_eq!(tokens_short(41_000), "41k");
        assert_eq!(tokens_short(2_340), "2.3k");
        assert_eq!(tokens_short(290_500), "291k");
        assert_eq!(tokens_short(1_200_000), "1.2M");
        assert_eq!(bytes_short(512), "512\u{a0}B");
        assert_eq!(bytes_short(12_595), "12.2\u{a0}KB");
        assert_eq!(bytes_short(1_468_006), "1.3\u{a0}MB");
        assert_eq!(duration_words(45_000), "45s");
        assert_eq!(duration_words(125_000), "2m 5s");
        assert_eq!(duration_words(6_710_000), "1h 51m");
        // A second part of nothing is left out.
        assert_eq!(duration_words(60_000), "1m");
        assert_eq!(duration_words(7_200_000), "2h");
        assert_eq!(duration_words(86_400_000), "1d");
    }
}
