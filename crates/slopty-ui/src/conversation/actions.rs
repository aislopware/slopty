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
        /// Ask the draft of a fork of the thread, in a sheet over it.
        AskAside,
        /// Open the screen the thread's agent drives beside it.
        WatchAgentScreen,
        /// Take the thread's exited agent up again.
        ResumeAgent,
        /// Answer the request that has the keyboard with its plain allow: bound only where the
        /// request itself is focused, so a request that arrives never changes what a key grants.
        AllowRequest,
        /// Answer the request that has the keyboard with its plain deny.
        DenyRequest,
    ]
);
