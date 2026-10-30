//! The agent's own prompt box as its TUI holds it, mirrored live for the inspector.
//!
//! The draft the person types and the pictures they pasted into it ([`Draft`],
//! [`super::Action::DraftSet`]), and the prompts they queued behind the turn under way
//! ([`super::Action::QueuedSet`]) (`docs/decisions/agents.md`, "TUI-first").
//!
//! Claude Code tells its draft through Slopty's mod, which only reads the box, and its queue
//! through its transcript. Nothing here is ever typed back into the TUI.

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use super::Image;

/// What the agent's prompt box holds now.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Draft {
    /// The words as the box shows them: a pasted picture as its placeholder (`[Image #2]`), a
    /// long paste as its own (`[Pasted text #1 +5 lines]`). Cut at [`Self::MAX_BYTES`].
    pub text: String,
    /// The pictures pasted into it, in the order their placeholders stand in [`Self::text`]. One
    /// whose file the agent has not written yet comes once it has.
    pub images: Vec<DraftImage>,
    /// When the worker heard it, by its own clock.
    pub at_ms: WallMs,
}

impl Draft {
    /// The most of a draft's words a thread holds, in bytes: what an inspector shows of a box
    /// longer than a screen.
    pub const MAX_BYTES: usize = 16 * 1024;
}

const _: () = assert!(
    Draft::MAX_BYTES >= 100 * 80 && Draft::MAX_BYTES <= crate::codec::MAX_FRAME_BYTES / 256,
    "a draft holds a box far taller than a screen whole (a hundred lines of eighty columns), \
     and one at its most stays a sliver of a frame beside the thread's other changes"
);

/// A picture pasted into the prompt box.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DraftImage {
    /// Its number in the box: the `N` of its `[Image #N]`.
    pub n: u32,
    /// The picture. Its bytes come by [`super::wire::ThreadRequest::Expand`] of its `at`, while
    /// the agent keeps the file.
    pub image: Image,
}
