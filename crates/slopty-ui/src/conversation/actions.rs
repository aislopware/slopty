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
        /// Open the commit sheet over the thread's or the review's tile.
        OpenCommit,
        /// Ask the branch's pull request again, where a thread's or a review's tile shows it.
        RefreshPullRequest,
        /// Switch the thread's model to think at the next level its agent offers, round.
        CycleEffort,
        /// Ask the draft of a fork of the thread, in a sheet over it.
        AskAside,
        /// Ask the thread's agent for its own review of the changes the review tile shows.
        ReviewWithAgent,
        /// Open the screen the thread's agent drives beside it.
        WatchAgentScreen,
    ]
);
