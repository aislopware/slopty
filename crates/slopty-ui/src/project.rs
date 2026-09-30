//! A project's board: the live view of an orchestration, shown in its orchestrator's tile.
//!
//! A project lives on the server (`docs/decisions/projects.md`); its orchestrator is a Claude
//! Code session in a terminal tile like any other. That tile turns between the TUI, the
//! conversation face and the board, so the board sits where the person talks to the
//! orchestrator, and every agent the tree names is a tile of its own that the board opens.
//!
//! * [`model`] — the server's projects mirrored, and what the board derives from one.
//! * `view` — the board itself: the header and its bar, what needs the person, and the tree, board
//!   and timeline lenses.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

pub mod model;
mod view;

pub use view::{AgentSeen, CTX, Node, ProjectEvent, ProjectView, Seen};

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
    ]
);

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
}

impl Lens {
    /// Its tab's words.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Tree => "Tree",
            Self::Board => "Board",
            Self::Timeline => "Timeline",
        }
    }

    /// Its tab's icon.
    #[must_use]
    pub const fn icon(self) -> IconName {
        match self {
            Self::Tree => IconName::ListTree,
            Self::Board => IconName::Kanban,
            Self::Timeline => IconName::Clock,
        }
    }

    /// Its tab's element name.
    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Tree => "project-lens-tree",
            Self::Board => "project-lens-board",
            Self::Timeline => "project-lens-timeline",
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
