//! The thread host: the worker's side of the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Every thread an adapter maps from an agent's session lives in the [`Host`]: its state, kept
//! by the one shared reducer, and its [`log`] on disk under the data directory
//! (`threads/<id>/`), numbered by epoch and sequence. A client follows a thread from the cursor
//! it holds ([`Follower`]) and is sent only what it missed, or a snapshot when the log no
//! longer holds the gap. The [`table`] keeps a row per thread for the lists that follow
//! nothing. Every intent is acted on once per id ([`intents`], [`Host::intent`]); what an
//! observed agent is sent is typed into its terminal by the [`Composer`].

pub mod claude;
pub mod compose;
pub mod follow;
pub mod host;
pub mod intents;
pub mod log;
pub mod review;
pub mod table;

pub use compose::Composer;
pub use follow::Follower;
pub use host::Host;
