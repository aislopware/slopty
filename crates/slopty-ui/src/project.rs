//! A project's board: the live view of an orchestration, shown in its orchestrator's tile.
//!
//! A project lives on the server (`docs/decisions/projects.md`); its orchestrator is a Claude
//! Code session in a terminal tile like any other. That tile turns between the TUI, the
//! conversation face and the board, so the board sits where the person talks to the
//! orchestrator, and every agent a card names is a tile of its own that the board opens.
//!
//! * [`create`] — the sheet that makes a project from a terminal its agent orchestrates.
//! * [`model`] — the server's projects mirrored, and what the board derives from one.
//! * [`recap`] — what changed since this client last looked.
//! * `view` — the board itself: the header and its bar, what needs the person, and the lanes each
//!   task stands in by what it waits on.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

pub mod create;
pub mod model;
pub mod recap;
mod view;

pub(crate) use view::{ALLOW, DENY};
pub use view::{AgentSeen, Asked, CTX, Node, ProjectEvent, ProjectView, Seen, WorkerSeen};

gpui::actions!(
    project,
    [
        /// Stand on the board's next row.
        SelectNext,
        /// Stand on the board's row above.
        SelectPrevious,
        /// Open the agent of the row the keyboard stands on.
        OpenNode,
        /// Start the task the keyboard stands on, not started yet.
        StartTask,
        /// Review the whole branch of the task the keyboard stands on, before it merges.
        ReviewTask,
        /// Ask for the merge of the task the keyboard stands on.
        MergeTask,
        /// Check the task the keyboard stands on again from the start.
        RetryTask,
        /// Tell the agent of the task the keyboard stands on to make its verifier pass.
        FixCi,
        /// Tell the agent of the task the keyboard stands on to address its review.
        AddressComments,
        /// Tell the agent of the task the keyboard stands on to resolve its conflicts.
        ResolveConflicts,
        /// Give up the task the keyboard stands on.
        CancelTask,
        /// Push the target after each merge, or stop.
        TogglePush,
        /// Turn the orchestrator's tile back to its terminal.
        ShowTerminal,
        /// Let the project go: its tasks, its queue, its timeline.
        DeleteProject,
        /// Put the keyboard on the message to the orchestrator.
        TellOrchestrator,
        /// Set the project's verifier command.
        EditChecks,
    ]
);

/// A board's key context, as bindings name it.
const BOARD: &[Option<&str>] = &[Some(CTX)];

/// The board's commands and their keys, beside the ones the keymap holds for it: a row's
/// actions on bare keys, as its other keys are, and what is rare or cannot be taken back on
/// none.
#[must_use]
pub fn key_bindings() -> Vec<crate::keymap::Command> {
    use crate::keymap::{Command, Scope};
    vec![
        Command::new(Scope::Project, "review_task", ReviewTask, &["v"], BOARD),
        Command::new(Scope::Project, "merge_task", MergeTask, &["m"], BOARD),
        Command::new(Scope::Project, "retry_task", RetryTask, &["r"], BOARD),
        Command::new(Scope::Project, "fix_ci", FixCi, &[], BOARD),
        Command::new(Scope::Project, "address_comments", AddressComments, &[], BOARD),
        Command::new(Scope::Project, "resolve_conflicts", ResolveConflicts, &[], BOARD),
        Command::new(Scope::Project, "cancel_task", CancelTask, &[], BOARD),
        Command::new(Scope::Project, "show_terminal", ShowTerminal, &["t"], BOARD),
        Command::new(Scope::Project, "tell_orchestrator", TellOrchestrator, &["c"], BOARD),
        Command::new(Scope::Project, "start_task", StartTask, &["s"], BOARD),
        Command::new(Scope::Project, "toggle_push", TogglePush, &[], BOARD),
        Command::new(Scope::Project, "delete_project", DeleteProject, &[], BOARD),
        Command::new(Scope::Project, "edit_checks", EditChecks, &[], BOARD),
    ]
}

/// The palette's lines for a project's board, with their keys.
#[must_use]
pub fn palette_items(bindings: &[gpui::KeyBinding]) -> Vec<crate::palette::PaletteItem> {
    let line = |label: &str, action: Box<dyn gpui::Action>| {
        crate::palette::PaletteItem::new(label, action, bindings)
    };
    vec![
        line("Review the task", Box::new(ReviewTask)),
        line("Merge the task", Box::new(MergeTask)),
        line("Retry the task", Box::new(RetryTask)),
        line("Tell the task's agent to fix CI", Box::new(FixCi)),
        line("Tell the task's agent to address the comments", Box::new(AddressComments)),
        line("Tell the task's agent to resolve the conflicts", Box::new(ResolveConflicts)),
        line("Cancel the task", Box::new(CancelTask)),
        line("Start the task", Box::new(StartTask)),
        line("Push after each merge", Box::new(TogglePush)),
        line("Show the orchestrator's terminal", Box::new(ShowTerminal)),
        line("Message the orchestrator\u{2026}", Box::new(TellOrchestrator)),
        line("Verifier\u{2026}", Box::new(EditChecks)),
        line("Delete the project", Box::new(DeleteProject)),
    ]
}

impl model::Lane {
    /// Its element name: `project-lane-needs-you`.
    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs-you",
            Self::Failed => "failed",
            Self::Working => "working",
            Self::UpNext => "up-next",
            Self::Verifying => "verifying",
            Self::ReadyToMerge => "ready-to-merge",
            Self::Merged => "merged",
        }
    }
}

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;
