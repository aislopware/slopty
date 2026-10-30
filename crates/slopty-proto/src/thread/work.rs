//! What a thread's agent does besides the conversation, as its TUI shows it.
//!
//! Its background tasks' figures and the notices they end with, and each call's running time and
//! the words it ended with.
//!
//! Every shape here is agent-neutral and open where agents differ: Claude Code's transcript
//! fills them (`docs/decisions/agents.md`, "TUI-first"), and
//! an agent that says none of it leaves them empty. Each arrives by its own [`super::Action`] so
//! a ticking figure never resends an item or a list.

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use super::{Clipped, ItemId, Retry};

/// A background task's figures so far ([`super::BackgroundTask::usage`], [`TaskNote::usage`]).
///
/// From the agent's `task_progress` or the notice it ended with. The TUI's agents list reads
/// "↓ 290.5k tokens" and "Done (12 tool uses · 41k tokens · 2m 5s)" from these.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TaskUsage {
    /// Tokens it took.
    pub tokens: u64,
    /// Tool calls it made.
    pub tool_uses: u64,
    /// How long it has run, paused time left out.
    pub duration_ms: u64,
}

/// A background task's notice, as a row of its own in the transcript ([`super::ItemBody::Task`]).
///
/// The TUI's `● Agent "x" finished · 2m 5s`, `● Monitor event: … · lane stall watch`,
/// `● Background command "y" failed with exit code 2`. Linked to its task ([`Self::task`]), which
/// the tray lists, and to the call that started it when the agent names one ([`Self::call`]): a
/// monitor's events name none, and are rows all the same.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskNote {
    /// The task, by the agent's id ([`super::BackgroundTask::id`]).
    pub task: String,
    /// What kind of task: [`super::BackgroundTask::kind`]'s words.
    pub kind: String,
    /// What it says of the task. Open: [`TaskNote::COMPLETED`], [`TaskNote::FAILED`],
    /// [`TaskNote::KILLED`], [`TaskNote::WAITING`], [`TaskNote::EVENT`], [`TaskNote::EXPIRED`].
    pub status: String,
    /// The task's description, muted on the right of the row ("lane stall watch").
    pub title: String,
    /// The row's words, as the TUI words them: "Agent \"Look\" is waiting on its own background
    /// work", "Monitor event: STALL lane WATCH idle 20 min", "Background command \"x\" completed
    /// (exit code 0)". The client adds " · {duration}" from [`Self::usage`].
    pub summary: String,
    /// What opens under the row: a monitor's event body, an agent's report or its interim
    /// result; `None` when there is nothing more.
    pub body: Option<Clipped>,
    /// The call that started the task, when the agent names it.
    pub call: Option<ItemId>,
    /// The task's figures as it ended or paused.
    pub usage: Option<TaskUsage>,
    /// A command's exit code, when it ended with one.
    pub exit_code: Option<i32>,
    /// The resources its result linked (an MCP task's `resource_link`s).
    pub links: Vec<crate::visual::ResourceLink>,
}

impl TaskNote {
    /// It finished.
    pub const COMPLETED: &'static str = "completed";
    /// A monitor's event: one of many, the task runs on.
    pub const EVENT: &'static str = "event";
    /// A monitor's watch ran out of time.
    pub const EXPIRED: &'static str = "expired";
    /// It failed.
    pub const FAILED: &'static str = "failed";
    /// It was stopped: by the person, by the agent, or by the machine.
    pub const KILLED: &'static str = "killed";
    /// An agent that stopped with background work of its own still running: its result may be
    /// interim, and the same task may notify again.
    pub const WAITING: &'static str = "waiting";
}

/// What the agent says of one call beside the call itself ([`super::Action::CallNoted`]).
///
/// How long it has run and, once it ended, how long its run alone took; the background task it
/// became, a retry of a subagent's request, the words it ended with, and its header where the
/// agent's own UI words it better than its kind.
/// The TUI's "Running… (12s)", "Running in the background", "Interrupted", "exit code 2",
/// "Denied by auto mode classifier · reason", "Read report.pdf · pages 1-3".
///
/// A note leaves what it does not say as the last one said it: a running time said after the
/// task keeps the task, and an ending or a run time said once stays.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct CallNote {
    /// How long it had run when the agent last said, in ms; a client ticks on from there.
    pub elapsed_ms: Option<u64>,
    /// How long it ran once it ended, in ms, by the agent's own measure: its run alone, the
    /// wait for the person's permission and the agent's own hooks before it left out (Claude
    /// Code's `PostToolUse` `duration_ms`). The call's wall time, from its record to its
    /// result's, holds both.
    pub ran_ms: Option<u64>,
    /// When the agent last said so.
    pub at_ms: WallMs,
    /// The background task it started or became.
    pub task: Option<String>,
    /// A subagent's request being tried again, inside this call.
    pub retry: Option<Retry>,
    /// How it ended, in the agent's words, when its state alone does not say ("Interrupted",
    /// "exit code 2", "Denied by auto mode classifier · rm is destructive", "Running in the
    /// background").
    pub said: Option<String>,
    /// The call's header in the agent's own UI words, where they say more than its kind's
    /// ("Read report.pdf · pages 1-3", "Update(src/lib.rs)", "Cron(*/5 * * * *: check CI)").
    #[serde(default)]
    pub title: Option<String>,
}

impl CallNote {
    /// This note over `had`, the one said before it: what this one leaves unsaid stays as
    /// `had` said it.
    #[must_use]
    pub fn over(self, had: &Self) -> Self {
        Self {
            ran_ms: self.ran_ms.or(had.ran_ms),
            task: self.task.or_else(|| had.task.clone()),
            said: self.said.or_else(|| had.said.clone()),
            title: self.title.or_else(|| had.title.clone()),
            ..self
        }
    }
}
