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
    AllowRequest, AskAside, CycleDensity, DenyRequest, EditLastQueued, Interrupt, QueueMessage,
    ResumeAgent, WatchAgentScreen,
};
pub use attach::Attach;

/// The thread's own lines in the palette.
///
/// The agent's screen, and every button of the thread's that the keyboard reaches no other way.
/// A line shows only where the focused thread answers it.
#[must_use]
pub fn palette_items(bindings: &[gpui::KeyBinding]) -> Vec<crate::palette::PaletteItem> {
    let line = |label: &str, action: Box<dyn gpui::Action>| {
        crate::palette::PaletteItem::new(label, action, bindings)
    };
    vec![
        line("Resume the agent", Box::new(ResumeAgent)),
        line("Watch the agent's screen", Box::new(WatchAgentScreen)),
        line("Ask aside", Box::new(AskAside)),
        line("Allow the request", Box::new(AllowRequest)),
        line("Deny the request", Box::new(DenyRequest)),
    ]
}

/// The key context the face binds in.
pub const CTX: &str = "Conversation";

/// The key context of a request that has the keyboard: its answers in a thread, a *Needs you*
/// row in the navigator. ⌘↵ and ⌘⌫ answer it there and nowhere else.
pub const REQUEST_CTX: &str = "Request";
