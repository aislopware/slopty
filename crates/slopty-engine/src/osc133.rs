//! The OSC 133 marks the engine keeps: where a prompt starts, where a command's output starts,
//! and where the command ends, with its status.
//!
//! libghostty-vt consumes OSC 133 for its per-row prompt flags, but a flag cannot tell two
//! prompts on adjacent rows apart (a command with no output), and a row has no status. So the
//! engine notes the cursor at each mark. libghostty reports every step the shell writes through
//! its semantic prompt effect, from inside `vt_write`: the terminal has applied the bytes before
//! the mark and none after it, so the cursor is where the shell wrote the mark. It reports what
//! its own parser accepted, so a mark cut off by CAN or SUB, cut short by an ESC, or longer
//! than any buffer of ours is counted exactly when the terminal acts on it.

use libghostty_vt::terminal::{PromptKind, SemanticPrompt, SemanticPromptKind};

/// The marks worth keeping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// A primary prompt starts on the cursor row (`133;A`, `N` or `P`, also a right prompt
    /// `k=r`). The rows of a continuation or secondary prompt (`k=c`, `k=s`) are rows of the
    /// same prompt and not reported.
    PromptStart,
    /// `133;C`: the command's output starts on the cursor row. libghostty takes the row out of
    /// the prompt on it but writes no cell, so the row would not be in a frame.
    OutputStart,
    /// `133;D`: the command ended.
    CommandEnd {
        /// The status the shell reported, when it did and it is one a process exits with.
        exit: Option<u8>,
    },
}

impl Mark {
    /// The mark a step the shell reported is, if it is one the engine keeps. Input start
    /// (`133;B`, `I`) is not: libghostty marks the input cells itself.
    #[must_use]
    pub fn of(event: SemanticPrompt<'_>) -> Option<Self> {
        match event.kind().ok()? {
            SemanticPromptKind::PromptStart => match event.prompt_kind().ok()? {
                PromptKind::Primary | PromptKind::Right => Some(Self::PromptStart),
                _ => None,
            },
            SemanticPromptKind::OutputStart => Some(Self::OutputStart),
            SemanticPromptKind::CommandEnd => Some(Self::CommandEnd {
                exit: event.exit_code().and_then(|c| u8::try_from(c).ok()),
            }),
            _ => None,
        }
    }
}
