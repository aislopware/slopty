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
pub mod sleep;
pub mod table;
pub mod terminals;

pub use compose::Composer;
pub use follow::Follower;
pub use host::Host;

/// A task's thread as the server seats it ([`crate::orchestrate::TaskThread`]).
///
/// The seat it is known by, what its Slopty tools are told, and the role it plays. Each adapter
/// hands these to its agent through the agent's own door.
///
/// The host keeps it with the thread ([`Host::seated`]), so a thread taken up again after the
/// worker restarts is given the same. It holds nothing secret: the worker's own variables for
/// the seat, its token among them, are added where the agent runs ([`Host::env_of`]).
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Seated {
    /// The seat: the session its Slopty tools speak as, and the terminal an agent that runs
    /// in one runs in.
    pub seat: slopty_core::SessionId,
    /// The server's variables for it: its project and task.
    pub env: Vec<(String, String)>,
    /// What the agent is told it is for.
    pub role: Option<String>,
    /// Slopty's CLI, whose `mcp` serves the tools; `None` where it is not found.
    pub relay: Option<String>,
}

/// What the worker gives a seat's agent and tools to find in their environment.
///
/// The server's variables for the seat, with the worker's own for it (its server, the seat as the
/// session, the token that proves it). The daemon gives it to the host ([`Host::set_seat_env`]).
pub type SeatEnv = std::sync::Arc<
    dyn Fn(slopty_core::SessionId, &[(String, String)]) -> Vec<(String, String)> + Send + Sync,
>;

impl Seated {
    /// The intent a start at this seat is acted on as: one per seat, so a start repeated after
    /// a dropped link answers with the thread the first one started.
    #[must_use]
    pub fn intent(&self) -> slopty_proto::thread::IntentId {
        let derived =
            slopty_proto::thread::ThreadId::derived(&["task seat", &self.seat.to_string()]);
        slopty_proto::thread::IntentId::from_uuid(*derived.as_uuid())
    }

    /// The first message of an agent that takes no role of its own: the role ahead of `prompt`.
    #[must_use]
    pub fn ahead(&self, prompt: Option<&str>) -> Option<String> {
        let prompt = prompt.map(str::trim).filter(|p| !p.is_empty());
        match (self.role.as_deref().map(str::trim).filter(|r| !r.is_empty()), prompt) {
            (Some(role), Some(prompt)) => Some(format!("{role}\n\n{prompt}")),
            (Some(role), None) => Some(role.to_owned()),
            (None, prompt) => prompt.map(str::to_owned),
        }
    }
}
