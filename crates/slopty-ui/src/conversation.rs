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
    AskAside, CycleDensity, CycleEffort, EditLastQueued, Interrupt, OpenCommit, QueueMessage,
    RefreshPullRequest, ReviewWithAgent, WatchAgentScreen,
};
pub use attach::Attach;

/// The thread's and the review's own lines in the palette: the commit sheet, the pull
/// request's refresh, the agent's own review and its screen, which no button carries a key for.
#[must_use]
pub fn palette_items(bindings: &[gpui::KeyBinding]) -> Vec<crate::palette::PaletteItem> {
    use crate::icons::IconName;
    vec![
        crate::palette::PaletteItem::new(
            "Commit\u{2026}",
            IconName::GitBranch,
            Box::new(OpenCommit),
            bindings,
        ),
        crate::palette::PaletteItem::new(
            "Review with the agent",
            IconName::ListChecks,
            Box::new(ReviewWithAgent),
            bindings,
        ),
        crate::palette::PaletteItem::new(
            "Watch the agent's screen",
            IconName::Monitor,
            Box::new(WatchAgentScreen),
            bindings,
        ),
        crate::palette::PaletteItem::new(
            "Refresh pull request",
            IconName::GitPullRequest,
            Box::new(RefreshPullRequest),
            bindings,
        ),
        crate::palette::PaletteItem::new(
            "Ask aside",
            IconName::MessageCircleQuestionMark,
            Box::new(AskAside),
            bindings,
        ),
        crate::palette::PaletteItem::new(
            "Next effort level",
            IconName::Brain,
            Box::new(CycleEffort),
            bindings,
        ),
    ]
}

/// The key context the face binds in.
pub const CTX: &str = "Conversation";
