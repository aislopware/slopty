//! The thread host: the worker's side of the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Every thread an adapter maps from an agent's session lives in the [`Host`]: its state, kept
//! by the one shared reducer, and its [`log`] on disk under the data directory
//! (`threads/<id>/`), numbered by epoch and sequence. A client follows a thread from the cursor
//! it holds ([`Follower`]) and is sent only what it missed, or a snapshot when the log no
//! longer holds the gap. The [`table`] keeps a row per thread for the lists that follow
//! nothing. Every intent is acted on once per id ([`intents`], [`Host::intent`]); what an
//! observed agent is sent is typed into its terminal by the [`Composer`]. A Codex thread is
//! followed over the person's Codex daemon instead ([`codex`]), and what it is sent goes there.
//! A pi thread is started here and driven over pi's RPC mode ([`pi`]), and the thread of any other
//! agent that speaks the Agent Client Protocol over ACP ([`acp`]). A Claude Code thread is
//! started by opening the person's `claude` in one of the worker's [`terminals`] and observing
//! it ([`claude::start`]); a Codex thread by asking the person's Codex daemon for one. The
//! person's past prompts, as the agents record them, are searched by [`history`].

pub mod acp;
pub mod attach;
pub mod claude;
pub mod codex;
pub mod compose;
pub mod follow;
pub mod fork;
pub mod history;
pub mod host;
pub mod intents;
pub mod log;
pub mod pi;
pub mod review;
pub mod table;
pub mod terminals;

pub use compose::Composer;
pub use follow::Follower;
pub use host::Host;
