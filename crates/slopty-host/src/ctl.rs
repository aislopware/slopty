//! Local control protocol between `slopty-hostd` and the `slopty` CLI: newline-delimited JSON
//! over a Unix socket, one request and one reply per connection.

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, SessionId};
use slopty_proto::terminal::SessionSummary;

/// CLI → daemon.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum CtlRequest {
    /// Mint a pairing ticket.
    Ticket,
    /// Identity and sessions.
    Status,
    /// Paired clients.
    Paired,
    /// Forget a client.
    Revoke {
        /// Endpoint id (hex).
        endpoint: String,
    },
    /// A coding-agent hook fired inside a session (relayed by `slopty hook`).
    Hook {
        /// The session the hook ran in (`SLOPTY_SESSION`).
        session: SessionId,
        /// The hook's stdin, verbatim JSON.
        payload: String,
    },
}

/// One paired client, as listed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PairedSummary {
    /// Endpoint id (hex).
    pub endpoint: String,
    /// App identity.
    pub client: ClientId,
    /// Name from its `Hello`.
    pub name: String,
    /// Unix seconds.
    pub paired_at: u64,
}

/// Daemon → CLI.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum CtlReply {
    /// A ticket string.
    Ticket {
        /// Base32 ticket.
        ticket: String,
    },
    /// Status.
    Status {
        /// Endpoint id (hex).
        id: String,
        /// Host name.
        name: String,
        /// Sessions.
        sessions: Vec<SessionSummary>,
    },
    /// Paired clients.
    Paired {
        /// The list.
        paired: Vec<PairedSummary>,
    },
    /// Done.
    Ok {
        /// Whether anything changed.
        changed: bool,
    },
    /// Failed.
    Error {
        /// Why.
        message: String,
    },
}
