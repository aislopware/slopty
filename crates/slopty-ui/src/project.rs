//! A project's board: the live view of an orchestration, shown in its orchestrator's tile.
//!
//! A project lives on the server (`docs/decisions/projects.md`); its orchestrator is a Claude
//! Code session in a terminal tile like any other. That tile turns between the TUI, the
//! conversation face and the board, so the board sits where the person talks to the
//! orchestrator, and every agent the tree names is a tile of its own that the board opens.
//!
//! * [`create`] — the sheet that makes a project from a terminal its agent orchestrates.
//! * [`model`] — the server's projects mirrored, and what the board derives from one.
//! * [`recap`] — what changed since this client last looked.
//! * [`spend`] — time at work per node and subtree, and what the agents' threads say they cost.
//! * `view` — the board itself: the header and its bar, what needs the person, and the tree, board
//!   and timeline lenses.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

pub mod create;
pub mod model;
pub mod recap;
pub mod spend;
mod view;

#[cfg(test)]
pub(crate) use view::BRIEF;
pub use view::{AgentSeen, CTX, Node, ProjectEvent, ProjectView, Seen, WorkerSeen};

use crate::icons::IconName;

gpui::actions!(
    project,
    [
        /// Stand on the board's next row.
        SelectNext,
        /// Stand on the board's row above.
        SelectPrevious,
        /// Open the agent of the row the keyboard stands on.
        OpenNode,
        /// The tree: who split what from whom, down to the subagents inside a session.
        ShowTree,
        /// The board: each task in the lane its most urgent descendant is in.
        ShowBoard,
        /// The timeline: what happened, newest first.
        ShowTimeline,
        /// The machines: each worker, how it is doing, and the project's agents on it.
        ShowMachines,
        /// Choose the worker the task the keyboard stands on runs on.
        RunTaskOn,
        /// Start the proposed task the keyboard stands on.
        StartTask,
        /// Start every task whose start is proposed.
        StartProposed,
        /// Hold each task's start for the person, or let the orchestrator start them.
        ToggleAskToStart,
        /// Ask for the merge of the task the keyboard stands on.
        MergeTask,
        /// Check the task the keyboard stands on again from the start.
        RetryTask,
        /// Approve the work of the task the keyboard stands on over its reviewer.
        ApproveTask,
        /// Tell the agent of the task the keyboard stands on to make its verifier pass.
        FixCi,
        /// Tell the agent of the task the keyboard stands on to address its review.
        AddressComments,
        /// Tell the agent of the task the keyboard stands on to resolve its conflicts.
        ResolveConflicts,
        /// Push the target again for the task the keyboard stands on, whose push failed.
        PushTask,
        /// Give up the task the keyboard stands on.
        CancelTask,
        /// End the terminal of the agent of the task the keyboard stands on.
        StopTaskAgent,
        /// Push the target after each merge, or stop.
        TogglePush,
        /// Turn the orchestrator's tile back to its terminal.
        ShowTerminal,
        /// Let the project go: its tasks, its queue, its timeline.
        DeleteProject,
        /// Put the keyboard on the line to the orchestrator.
        TellOrchestrator,
        /// Set the project's verifier command and whether a reviewer reads each task's work.
        EditChecks,
        /// Set what the project's agents may spend.
        EditBudget,
        /// Make a project of the focused terminal's directory, with that terminal as its
        /// orchestrator.
        StartProject,
    ]
);

/// A board's key context, as bindings name it.
const BOARD: &[Option<&str>] = &[Some(CTX)];
/// The workspace's, for what a board need not be shown for.
const WORKSPACE: &[Option<&str>] = &[Some("Workspace && !Screen")];

/// The board's commands and their keys, beside the ones the keymap holds for it: a row's
/// actions on bare keys, as its other keys are, and what is rare or cannot be taken back on
/// none.
#[must_use]
pub fn key_bindings() -> Vec<crate::keymap::Command> {
    use crate::keymap::{Command, Scope};
    vec![
        Command::new(Scope::Project, "merge_task", MergeTask, &["m"], BOARD),
        Command::new(Scope::Project, "retry_task", RetryTask, &["r"], BOARD),
        Command::new(Scope::Project, "approve_task", ApproveTask, &["a"], BOARD),
        Command::new(Scope::Project, "fix_ci", FixCi, &[], BOARD),
        Command::new(Scope::Project, "address_comments", AddressComments, &[], BOARD),
        Command::new(Scope::Project, "resolve_conflicts", ResolveConflicts, &[], BOARD),
        Command::new(Scope::Project, "push_task", PushTask, &[], BOARD),
        Command::new(Scope::Project, "cancel_task", CancelTask, &[], BOARD),
        Command::new(Scope::Project, "stop_task_agent", StopTaskAgent, &[], BOARD),
        Command::new(Scope::Project, "show_terminal", ShowTerminal, &["t"], BOARD),
        Command::new(Scope::Project, "tell_orchestrator", TellOrchestrator, &["c"], BOARD),
        Command::new(Scope::Project, "show_machines", ShowMachines, &["4"], BOARD),
        Command::new(Scope::Project, "run_task_on", RunTaskOn, &["o"], BOARD),
        Command::new(Scope::Project, "start_task", StartTask, &["s"], BOARD),
        Command::new(Scope::Project, "start_proposed", StartProposed, &[], BOARD),
        Command::new(Scope::Project, "toggle_ask_to_start", ToggleAskToStart, &[], BOARD),
        Command::new(Scope::Project, "toggle_push", TogglePush, &[], BOARD),
        Command::new(Scope::Project, "delete_project", DeleteProject, &[], BOARD),
        Command::new(Scope::Project, "edit_checks", EditChecks, &[], BOARD),
        Command::new(Scope::Project, "edit_budget", EditBudget, &[], BOARD),
        Command::new(Scope::Workspace, "start_project", StartProject, &[], WORKSPACE),
    ]
}

/// The palette's lines for a project's board and for starting one, with their keys.
#[must_use]
pub fn palette_items(bindings: &[gpui::KeyBinding]) -> Vec<crate::palette::PaletteItem> {
    let line = |label: &str, icon: IconName, action: Box<dyn gpui::Action>| {
        crate::palette::PaletteItem::new(label, icon, action, bindings)
    };
    vec![
        line("Start a project here", IconName::Workflow, Box::new(StartProject)),
        line("Merge the task", IconName::GitBranch, Box::new(MergeTask)),
        line("Retry the task", IconName::RotateCw, Box::new(RetryTask)),
        line("Approve the task's work", IconName::Check, Box::new(ApproveTask)),
        line("Tell the task's agent to fix CI", IconName::Wrench, Box::new(FixCi)),
        line(
            "Tell the task's agent to address the comments",
            IconName::MessageSquare,
            Box::new(AddressComments),
        ),
        line(
            "Tell the task's agent to resolve the conflicts",
            IconName::GitBranch,
            Box::new(ResolveConflicts),
        ),
        line("Push the task's merge again", IconName::Upload, Box::new(PushTask)),
        line("Cancel the task", IconName::X, Box::new(CancelTask)),
        line("Stop the task's agent", IconName::Square, Box::new(StopTaskAgent)),
        line("Run the task on\u{2026}", IconName::Server, Box::new(RunTaskOn)),
        line("Start the task", IconName::CircleDot, Box::new(StartTask)),
        line("Start every proposed task", IconName::ListChecks, Box::new(StartProposed)),
        line("Ask before each task starts", IconName::Hand, Box::new(ToggleAskToStart)),
        line("Show the machines", IconName::Server, Box::new(ShowMachines)),
        line("Push after each merge", IconName::Upload, Box::new(TogglePush)),
        line("Show the orchestrator's terminal", IconName::SquareTerminal, Box::new(ShowTerminal)),
        line("Tell the orchestrator\u{2026}", IconName::MessageSquare, Box::new(TellOrchestrator)),
        line("Verifier and review\u{2026}", IconName::ListChecks, Box::new(EditChecks)),
        line("Budget\u{2026}", IconName::Activity, Box::new(EditBudget)),
        line("Delete the project", IconName::X, Box::new(DeleteProject)),
    ]
}

/// One way of looking at a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Lens {
    /// Who split what from whom.
    #[default]
    Tree,
    /// What needs the person, what runs, what waits to merge.
    Board,
    /// What happened, newest first.
    Timeline,
    /// Where everything runs: each worker and the agents on it.
    Machines,
}

impl Lens {
    /// Its tab's words.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Tree => "Tree",
            Self::Board => "Board",
            Self::Timeline => "Timeline",
            Self::Machines => "Machines",
        }
    }

    /// Its tab's icon.
    #[must_use]
    pub const fn icon(self) -> IconName {
        match self {
            Self::Tree => IconName::ListTree,
            Self::Board => IconName::Kanban,
            Self::Timeline => IconName::Clock,
            Self::Machines => IconName::Server,
        }
    }

    /// Its tab's element name.
    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Tree => "project-lens-tree",
            Self::Board => "project-lens-board",
            Self::Timeline => "project-lens-timeline",
            Self::Machines => "project-lens-machines",
        }
    }
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
