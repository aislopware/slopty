//! slopty: the inbox (`docs/decisions/layout.md`, "Attention is an inbox and a jump, never a
//! feed"): the titlebar's eye, and behind it what needs the person. Its list, its keys and its
//! button land with lane L's step 4; this is its place in the shell.

/// The inbox's state, one per shell.
#[derive(Default)]
pub(super) struct Inbox {
    open: bool,
}

impl Inbox {
    /// Whether it is up: it holds the keyboard.
    pub(super) fn is_open(&self) -> bool {
        self.open
    }
}
