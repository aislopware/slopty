//! The conversation face of a Claude Code terminal: the agent's session read as a structured
//! transcript, toggled per tile with the TUI, over the same PTY and the same session.
//!
//! The TUI stays the source of truth. The face is a projection of what the worker decodes from
//! the transcript, the hooks and the status line (`slopty_proto::conversation`), a composer
//! that types into the same PTY, and a card for the permission prompts the worker holds while a
//! client shows the face. Nothing here drives the agent behind its back.
//!
//! * [`model`] — the conversation as received: threads, live blocks, meters, pending messages.
//! * [`rows`] — a thread as the list's rows: turns folded once settled, calls grouped, densities.
//! * [`tools`] — each tool's title line and what a group of calls adds up to.
//! * [`diff`] — an edit's patch numbered and coloured, in a column or side by side.
//! * [`figures`] — a turn's figures, times of day and the files a run of entries changed.
//! * [`find`] — which entries hold a query, and the row that shows each.
//! * [`composer`] — what the composer types into the terminal, and the files attached to it.
//! * [`approval`] — a held permission prompt and how it ended.
//! * [`question`] — an `AskUserQuestion` answered in the composer, one question at a time.
//! * [`menu`] — the composer's slash command and `@` mention menus, as text.
//! * [`view`] — the face itself: the list, the prompt rail, the task card, the composer.
//! * [`lines`] — a diff's lines drawn, for the thread view and the review tile.
//! * [`thread`] — the thread view over the agent-neutral model, for every agent.

mod actions;
pub mod approval;
pub mod chips;
pub mod composer;
pub mod diff;
pub mod figures;
pub mod find;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod lines;
pub mod menu;
pub mod model;
pub mod question;
pub mod rows;
pub mod thread;
pub mod tools;
pub mod view;

pub use actions::{CycleDensity, Interrupt};
pub use composer::Attach;
pub use view::{ConversationView, FaceEvent, HeaderChips};

/// The key context the face binds in.
pub const CTX: &str = "Conversation";

/// What the composer says before anything is typed.
pub const MESSAGE_PLACEHOLDER: &str = "Message Claude";
