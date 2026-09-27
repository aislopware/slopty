//! The face's own actions; the workspace's key table binds them in [`super::CTX`].

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

gpui::actions!(
    conversation,
    [
        /// Show more or less of the agent's work: Normal, Thinking, Verbose.
        CycleDensity,
        /// Stop the agent's turn, as Esc in its terminal does.
        Interrupt,
    ]
);
