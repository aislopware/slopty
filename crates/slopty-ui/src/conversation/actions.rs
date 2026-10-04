//! The face's own actions; the workspace's key table binds them in [`super::CTX`].

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

gpui::actions!(
    conversation,
    [
        /// Show more or less of the agent's work: Normal, Thinking, Verbose.
        CycleDensity,
        /// Stop the agent's turn, as Esc in its terminal does.
        Interrupt,
        /// Send the thread's draft once the turn under way ends.
        QueueMessage,
        /// Take the last waiting message into the thread's composer to change it.
        EditLastQueued,
        /// Choose when the thread's draft goes: at a time, or once another thread rests.
        SendLater,
    ]
);
