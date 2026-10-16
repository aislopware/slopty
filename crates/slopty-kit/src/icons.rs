//! How a thing stands, in one closed list: an agent, a command, a Mac's setup step. The shell
//! draws it (its status dots, spinners and checks); this names it.

/// How something stands.
///
/// Waiting, idle and a worker out of reach are quiet; "needs you" is the one that asks for the
/// person, so nothing else is said as loudly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Nothing to say: a shell at its prompt, an agent at rest.
    Idle,
    /// Busy on its own: an agent thinking or running a tool, a remote picture on its way.
    Working,
    /// *Waiting*: a shell's command running for a while, or an agent's turn paused on work in
    /// the background. Busy, but nothing to watch for.
    Running,
    /// Waiting on the human: a permission, a question, an elicitation.
    NeedsYou,
    /// Finished and not yet looked at.
    Done,
    /// Failed: a command's non-zero exit, a session that ended in error.
    Failed,
    /// Out of reach: a worker away, a session reconnecting.
    Away,
}
