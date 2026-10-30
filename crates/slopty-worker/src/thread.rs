//! The thread host: the worker's side of the agent-neutral thread model
//! (`slopty_proto::thread`).
//!
//! Every thread the worker reads of an agent's session lives in the [`Host`]: its state, kept
//! by the one shared reducer, and its [`log`] on disk under the data directory
//! (`threads/<id>/`), numbered by epoch and sequence. A client follows a thread from the cursor
//! it holds ([`Follower`]) and is sent only what it missed, or a snapshot when the log no
//! longer holds the gap. The [`table`] keeps a row per thread for the lists that follow
//! nothing. Every intent is acted on once per id ([`intents`], [`Host::intent`]).
//!
//! An agent runs in its own TUI in a terminal (`docs/decisions/agents.md`, "TUI-first"): a
//! start opens it there ([`start`]), and every Claude Code in any terminal is read as threads
//! from its transcript and its hooks ([`claude`]). The person's past prompts, as the agents
//! record them, are searched by [`history`]. The windows and displays an agent drives are named
//! from its tool calls by [`screens`].

pub mod assets;
pub mod claude;
pub mod follow;
pub mod history;
pub mod host;
pub mod intents;
pub mod log;
pub mod marks;
pub mod review;
pub mod screens;
pub mod start;
pub mod table;

pub use follow::Follower;
pub use host::Host;

/// Why the person's words may not be typed into the terminal of a thread at `status` now.
///
/// Its agent waits on the person at a dialog, which would take the Enter of an `agent_tell` as
/// an answer. `None` when they may.
#[must_use]
pub fn submit_refused(status: &slopty_proto::thread::Status) -> Option<String> {
    if status.phase != slopty_proto::thread::Phase::NeedsYou {
        return None;
    }
    let asks = status.wait.as_ref().map(|w| w.text.trim()).filter(|t| !t.is_empty());
    Some(match asks {
        Some(asks) => format!("It waits for you in its terminal ({asks}): answer it there first"),
        None => "It waits for you in its terminal: answer it there first".to_owned(),
    })
}

/// An agent another agent started, as the server seats it ([`crate::orchestrate::SeatedThread`]).
///
/// The seat it is known by, the facts its row carries from the start (its parent), and the
/// system text it is given. Each adapter hands these to its agent through the agent's own door.
///
/// The host keeps it with the thread ([`Host::seated`]), so a thread taken up again after the
/// worker restarts is given the same. It holds nothing secret: the worker's own variables for
/// the seat, its token among them, are the seat's terminal's
/// ([`start::Terminals::env`]).
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Seated {
    /// The seat: the session its Slopty tools speak as.
    pub seat: slopty_core::SessionId,
    /// Open facts its row carries from the start ([`slopty_proto::agent::PARENT_FACT`]).
    pub facts: Vec<(String, String)>,
    /// Extra system text: what Slopty's tools reach.
    pub system: Option<String>,
}

impl Seated {
    /// The intent a start at this seat is acted on as: one per seat, so a start repeated after
    /// a dropped link answers with the thread the first one started.
    #[must_use]
    pub fn intent(&self) -> slopty_proto::thread::IntentId {
        let derived = slopty_proto::thread::ThreadId::derived(&["seat", &self.seat.to_string()]);
        slopty_proto::thread::IntentId::from_uuid(*derived.as_uuid())
    }

    /// The first message of an agent that takes no system text of its own: the system text
    /// ahead of `prompt`.
    #[must_use]
    pub fn ahead(&self, prompt: Option<&str>) -> Option<String> {
        let prompt = prompt.map(str::trim).filter(|p| !p.is_empty());
        match (self.system.as_deref().map(str::trim).filter(|r| !r.is_empty()), prompt) {
            (Some(system), Some(prompt)) => Some(format!("{system}\n\n{prompt}")),
            (Some(system), None) => Some(system.to_owned()),
            (None, prompt) => prompt.map(str::to_owned),
        }
    }
}
