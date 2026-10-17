//! slopty: a chat's background work, as the tray above its composer lists it (Slopty's own; not
//! in zeron).
//!
//! The view model serves it on `slopty.WatchWork` ([`crate::calls::WATCH_WORK`]) for one chat:
//! one [`WorkTray`] each time anything in it changes. It is the agent's own background tasks
//! (commands, monitors, Claude Code's subagents, workflows, MCP tasks) beside the child chats
//! another agent started under this one through Slopty, merged into one list, so a long run never
//! loses sight of what it left running (`docs/decisions/ui.md`, "Subagents live in their parent's
//! chat").
//!
//! Times are milliseconds since the epoch; a live row's elapsed time is the shell's to tick from
//! [`WorkRow::started_at`]. Words the person reads ([`WorkRow::state_words`],
//! [`WorkRow::figures`], [`WorkTray::summary`]) are said here once so every surface (the tray's
//! header, the footer under the composer, a card's step line) says them alike.

use serde::{Deserialize, Serialize};

/// A chat's background work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTray {
    /// The chat's own agent, the agents list's first row ("main").
    pub main: MainRow,
    /// Its work, in the order a tray lists it: what needs the person first, then what is at
    /// work by when it began, then what ended, the latest first ([`WorkTray::ENDED_KEPT`] at
    /// most). A task another task started follows its parent ([`WorkRow::parent`],
    /// [`WorkRow::depth`]).
    pub rows: Vec<WorkRow>,
    /// The live rows by kind, housekeeping left out.
    pub counts: WorkCounts,
    /// The counts in words, as the tray's header and the footer say them: "2 commands · 1
    /// subagent"; empty while nothing is at work.
    pub summary: String,
    /// Whether "Stop all" has anything to stop: a live row that can be stopped.
    pub can_stop_all: bool,
}

impl WorkTray {
    /// How many ended rows a tray keeps, the latest first.
    pub const ENDED_KEPT: usize = 8;
}

/// The chat's own agent as the agents list's first row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MainRow {
    /// A turn is under way.
    pub working: bool,
    /// What it is doing, in a phrase: its own summary, the hook it runs, the task in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Output tokens it has written in the turn so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// When the turn under way began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
}

/// One piece of background work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRow {
    /// Its id, which `slopty.StopWork` takes: `task:{id}` for one of the agent's own tasks,
    /// `chat:{chatId}` for a child chat no task stands for.
    pub id: String,
    /// What kind of work: [`WorkRow::COMMAND`], [`WorkRow::MONITOR`], [`WorkRow::SUBAGENT`],
    /// [`WorkRow::WORKFLOW`], [`WorkRow::MCP`], [`WorkRow::REMOTE`], [`WorkRow::TEAMMATE`], else
    /// the agent's own word.
    pub kind: String,
    /// The kind as a noun ("command", "subagent", "MCP task"), for the header and for assistive
    /// technologies.
    pub noun: String,
    /// What it is: the command, the monitor's description, the subagent's description.
    pub title: String,
    /// A subagent's kind (`general-purpose`, `Explore`), the agents list's first word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// The model a subagent of that kind runs on, when the agent names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Where it stands, as the agent says: `running`, `pending`, `paused`, `waiting`,
    /// `completed`, `failed`, `killed`, or a child chat's `needs-you`.
    pub state: String,
    /// Where it stands in the tray's words: "running", "waiting", "paused", "needs you", "done",
    /// "error", "stopped".
    pub state_words: String,
    /// It is still at work in any way.
    pub live: bool,
    /// It brings the agent news while it runs (a monitor's events, a subagent's result, work the
    /// turn waits on), so its dot pulses; a command left running beside the turn (a dev server)
    /// keeps a still dot.
    pub wakes: bool,
    /// The turn waits on it rather than going on beside it.
    pub awaited: bool,
    /// Housekeeping of the agent's own (a memory pass), which a tray folds away; left out of
    /// the counts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ambient: bool,
    /// What it asks of the person, while a child chat waits on them: the request's line. Such a
    /// row sits first, amber, and its request is answered in this chat's dock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you: Option<String>,
    /// What it is doing now, in a phrase: a subagent's own progress summary, else its last tool
    /// ("Running full check").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Tokens it took so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Tool calls it made so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<u64>,
    /// How long it ran, paused time left out, as the agent last said; a live row ticks from
    /// [`Self::started_at`] instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Its figures in words, time left out: "12 tool uses · 41k tokens"; empty when it has none.
    pub figures: String,
    /// When it began.
    pub started_at: i64,
    /// When it ended, once it has and the agent says when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    /// How it ended, in the agent's words ("exit code 2", "was stopped by user").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The end of what it printed, for a command or a monitor, as the agent last read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// A monitor's or a command's time limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// A monitor that watches until it is stopped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub persistent: bool,
    /// The transcript part of the call that started it (the tool part's id), which a click on
    /// the row scrolls to and expands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_part_id: Option<String>,
    /// The child chat its run is, where Slopty keeps one: what expands in place and drills in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// The row that started it, for work a subagent started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// How deep under a top row it hangs: 0 for a top row.
    #[serde(default)]
    pub depth: u32,
    /// Whether it can be stopped from here now (`slopty.StopWork`).
    pub can_stop: bool,
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

/// The live work of a chat, by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkCounts {
    /// Commands left running.
    pub commands: u32,
    /// Monitors watching.
    pub monitors: u32,
    /// Subagents at work.
    pub subagents: u32,
    /// Workflows running.
    pub workflows: u32,
    /// MCP tasks running.
    pub mcp: u32,
    /// Agents running elsewhere.
    pub remote: u32,
    /// Teammates at work.
    pub teammates: u32,
    /// Work of a kind the agent names that none of these is.
    pub other: u32,
    /// Rows that wait on the person.
    pub needs_you: u32,
}

impl WorkCounts {
    /// The live work in all.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.commands
            .saturating_add(self.monitors)
            .saturating_add(self.subagents)
            .saturating_add(self.workflows)
            .saturating_add(self.mcp)
            .saturating_add(self.remote)
            .saturating_add(self.teammates)
            .saturating_add(self.other)
    }

    /// The counts in words, the busiest kinds' order fixed: "2 commands · 1 subagent", "1
    /// monitor"; empty with nothing at work.
    #[must_use]
    pub fn summary(&self) -> String {
        let parts = [
            (self.commands, "command", "commands"),
            (self.monitors, "monitor", "monitors"),
            (self.subagents, "subagent", "subagents"),
            (self.workflows, "workflow", "workflows"),
            (self.mcp, "MCP task", "MCP tasks"),
            (self.remote, "remote agent", "remote agents"),
            (self.teammates, "teammate", "teammates"),
            (self.other, "background task", "background tasks"),
        ];
        parts
            .iter()
            .filter(|(n, ..)| *n > 0)
            .map(|(n, one, many)| format!("{n} {}", if *n == 1 { one } else { many }))
            .collect::<Vec<_>>()
            .join(" · ")
    }
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
    let tenths = |unit: u64| bytes.saturating_mul(10) / unit;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The header's words count by kind in a fixed order, singular and plural.
    #[test]
    fn counts_read_as_the_header_says_them() {
        let counts = WorkCounts { commands: 2, subagents: 1, ..WorkCounts::default() };
        assert_eq!(counts.summary(), "2 commands · 1 subagent");
        assert_eq!(counts.total(), 3);
        assert_eq!(WorkCounts::default().summary(), "");
        let mixed = WorkCounts { monitors: 1, mcp: 3, ..WorkCounts::default() };
        assert_eq!(mixed.summary(), "1 monitor · 3 MCP tasks");
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
