//! What changed in a project since this client last looked at its board, in a few lines.
//!
//! The cursor is the last timeline entry the board showed here ([`Looked`]); the recap reads
//! the entries after it and keeps what moves the person: a verifier or a step that failed, an
//! agent that ended, and then what got done. Each line names its tasks, most recent last, so the
//! board can say "Merged #4 and #6".

use slopty_core::WallMs;
use slopty_proto::project::{
    ChecksState, Moment, StepKind, StepState, TaskId, TaskState, TimelineEntry,
};

use super::model::Board;

/// How many tasks a line names before it counts the rest.
const NAMED: usize = 3;

/// How far this client read a project's timeline: the last entry its board showed, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Looked {
    /// The entry's `seq`; 0 when the timeline was empty.
    pub seq: u64,
    /// When the board stopped showing it, by this client's clock.
    pub at_ms: WallMs,
}

/// One kind of thing a recap tells, in the order it is told: what needs the person first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecapKind {
    /// Its verifier failed.
    VerifyFailed,
    /// Its work does not rebase onto the target.
    Conflicts,
    /// Its pull request's own checks failed.
    ChecksFailed,
    /// A step the server takes failed: a clone, bringing the branch home, a merge.
    StepFailed,
    /// Its agent ended before the work merged.
    AgentEnded,
    /// Its work merged.
    Merged,
    /// Its verifier passed.
    Verified,
    /// An agent started on it.
    Started,
    /// It was made.
    Created,
}

impl RecapKind {
    /// How a line of it begins.
    const fn words(self) -> &'static str {
        match self {
            Self::VerifyFailed => "Verifier failed on",
            Self::Conflicts => "Conflicts on",
            Self::ChecksFailed => "Checks failed on",
            Self::StepFailed => "A step failed on",
            Self::AgentEnded => "Agent ended on",
            Self::Merged => "Merged",
            Self::Verified => "Verified",
            Self::Started => "Started",
            Self::Created => "Created",
        }
    }

    /// Whether it asks something of the person.
    #[must_use]
    pub const fn needs_you(self) -> bool {
        matches!(
            self,
            Self::VerifyFailed
                | Self::Conflicts
                | Self::ChecksFailed
                | Self::StepFailed
                | Self::AgentEnded
        )
    }

    /// What a timeline entry tells, if a recap keeps it.
    fn of(what: &Moment) -> Option<Self> {
        match what {
            Moment::Verified(run) if !run.passed => Some(Self::VerifyFailed),
            Moment::Verified(_) => Some(Self::Verified),
            Moment::Checks(checks) if checks.state == ChecksState::Failing => {
                Some(Self::ChecksFailed)
            }
            Moment::Step(step) => match (step.kind, &step.state) {
                (StepKind::Verify, StepState::Failed { .. }) => Some(Self::VerifyFailed),
                (StepKind::Rebase, StepState::Failed { .. }) => Some(Self::Conflicts),
                (_, StepState::Failed { .. }) => Some(Self::StepFailed),
                _ => None,
            },
            Moment::AgentGone { .. } => Some(Self::AgentEnded),
            Moment::State { to: TaskState::Merged, .. } => Some(Self::Merged),
            Moment::Assigned { spawned: true, .. } => Some(Self::Started),
            Moment::TaskCreated { .. } => Some(Self::Created),
            _ => None,
        }
    }
}

/// One line of a recap: a kind, and the tasks it happened to, each once, in the order it
/// last happened to them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecapLine {
    /// What happened.
    pub kind: RecapKind,
    /// To which tasks.
    pub tasks: Vec<TaskId>,
}

impl RecapLine {
    /// The line in words: "Merged #4 Write the decision", "Merged #4, #6 and #9", "Merged #4,
    /// #6, #9 and 2 more".
    #[must_use]
    pub fn text(&self, board: &Board) -> String {
        let words = self.kind.words();
        match self.tasks.as_slice() {
            [one] => match board.tasks.get(one) {
                Some(card) => format!("{words} #{one} {}", card.title),
                None => format!("{words} #{one}"),
            },
            tasks => {
                let named: Vec<String> =
                    tasks.iter().take(NAMED).map(|t| format!("#{t}")).collect();
                let rest = tasks.len().saturating_sub(NAMED);
                match (named.split_last(), rest) {
                    (Some((last, first)), 0) if !first.is_empty() => {
                        format!("{words} {} and {last}", first.join(", "))
                    }
                    (_, 0) => format!("{words} {}", named.join(", ")),
                    _ => format!("{words} {} and {rest} more", named.join(", ")),
                }
            }
        }
    }
}

/// What changed in a project since this client last looked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recap {
    /// When this client last looked.
    pub since: Looked,
    /// What happened, what needs the person first.
    pub lines: Vec<RecapLine>,
    /// The entries since then could not all be read: the server keeps fewer, or the recap
    /// read only so far back.
    pub partial: bool,
}

impl Recap {
    /// The recap of `entries` after `since`, or `None` when nothing worth telling happened.
    /// An agent that ended once its work was done, as `board` has it now or as the entries
    /// say, ended as it should, and is not told.
    #[must_use]
    pub fn of<'a>(
        board: &Board,
        since: Looked,
        entries: impl IntoIterator<Item = &'a TimelineEntry>,
        partial: bool,
    ) -> Option<Self> {
        let mut lines: Vec<RecapLine> = Vec::new();
        for entry in entries.into_iter().filter(|e| e.seq > since.seq) {
            let (Some(task), Some(kind)) = (entry.task, RecapKind::of(&entry.what)) else {
                continue;
            };
            match lines.iter_mut().find(|l| l.kind == kind) {
                Some(line) => {
                    line.tasks.retain(|t| *t != task);
                    line.tasks.push(task);
                }
                None => lines.push(RecapLine { kind, tasks: vec![task] }),
            }
        }
        let merged: Vec<TaskId> = lines
            .iter()
            .filter(|l| l.kind == RecapKind::Merged)
            .flat_map(|l| l.tasks.iter().copied())
            .collect();
        for line in lines.iter_mut().filter(|l| l.kind == RecapKind::AgentEnded) {
            line.tasks.retain(|t| {
                let done = board.tasks.get(t).is_some_and(|c| {
                    matches!(c.state, TaskState::Verifying | TaskState::Done | TaskState::Merged)
                });
                !done && !merged.contains(t)
            });
        }
        lines.retain(|l| !l.tasks.is_empty());
        lines.sort_by_key(|l| l.kind);
        (!lines.is_empty()).then_some(Self { since, lines, partial })
    }
}
