//! An agent's conversation: the thread view over the agent-neutral thread model, for every
//! agent, and what it is drawn from.
//!
//! * [`thread`] — the thread view, its hub and the rows it draws.
//! * [`diff`] — an edit's patch numbered and coloured, in a column or side by side.
//! * [`lines`] — a diff's lines drawn, for the thread view and the review tile.
//! * [`figures`] — a model's name as a person says it, and times of day.
//! * [`attach`] — what a composer attaches: pasted pictures and dropped files, as chips.
//! * [`chips`] — those chips drawn.
//! * [`menu`] — the composer's slash command and `@` mention menus, as text.

mod actions;
pub mod attach;
pub mod chips;
pub mod diff;
pub mod figures;
pub mod lines;
pub mod menu;
pub mod thread;

pub use actions::{
    AskAside, BranchFromHere, CompactContext, CycleDensity, CycleEffort, EditLastQueued, Interrupt,
    OpenCommit, QueueMessage, RefreshPullRequest, ResumeAgent, ReviewChanges, ReviewWithAgent,
    ShowAgentTerminal, TakeBack, WatchAgentScreen,
};
pub use attach::Attach;

/// The thread's and the review's own lines in the palette.
///
/// The commit sheet, the pull request's refresh, the agent's own review and its screen, and
/// every button of the thread's that the keyboard reaches no other way. A line shows only where
/// the focused thread or review answers it.
#[must_use]
pub fn palette_items(bindings: &[gpui::KeyBinding]) -> Vec<crate::palette::PaletteItem> {
    use crate::icons::IconName;
    let line = |label: &str, icon: IconName, action: Box<dyn gpui::Action>| {
        crate::palette::PaletteItem::new(label, icon, action, bindings)
    };
    vec![
        line("Review changes", IconName::FilePen, Box::new(ReviewChanges)),
        line("Show the agent's terminal", IconName::SquareTerminal, Box::new(ShowAgentTerminal)),
        line("Take back from the terminal", IconName::Undo2, Box::new(TakeBack)),
        line("Compact context", IconName::Shrink, Box::new(CompactContext)),
        line("Branch from here\u{2026}", IconName::GitBranch, Box::new(BranchFromHere)),
        line("Resume the agent", IconName::Play, Box::new(ResumeAgent)),
        line("Commit\u{2026}", IconName::GitBranch, Box::new(OpenCommit)),
        line("Review with the agent", IconName::ListChecks, Box::new(ReviewWithAgent)),
        line("Watch the agent's screen", IconName::Monitor, Box::new(WatchAgentScreen)),
        line("Refresh pull request", IconName::GitPullRequest, Box::new(RefreshPullRequest)),
        line("Ask aside", IconName::MessageCircleQuestionMark, Box::new(AskAside)),
        line("Next effort level", IconName::Brain, Box::new(CycleEffort)),
    ]
}

/// The key context the face binds in.
pub const CTX: &str = "Conversation";
