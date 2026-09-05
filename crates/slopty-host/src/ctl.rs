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
    /// Health: permissions, reach, port, connected clients (`slopty host doctor`).
    Doctor,
    /// Live screen streams and their host-side counters (`slopty bench screen` reads the
    /// capture and encode latency through this on loopback).
    Screens,
    /// A coding-agent hook fired inside a session (relayed by `slopty hook`).
    Hook {
        /// The session the hook ran in (`SLOPTY_SESSION`).
        session: SessionId,
        /// The hook's stdin, verbatim JSON.
        payload: String,
    },
}

/// What `slopty host doctor` shows: the daemon's own view of its permissions and links.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Health {
    /// Daemon version.
    pub version: String,
    /// Path of the daemon binary (which is what TCC grants permissions to).
    pub exe: String,
    /// Screen Recording granted (windows and displays can be streamed).
    pub screen_recording: bool,
    /// Accessibility / post-event access granted (remote-window input is delivered).
    pub post_events: bool,
    /// `Anywhere` or `DirectOnly`.
    pub reach: String,
    /// Bound UDP port.
    pub port: u16,
    /// Clients connected right now.
    pub clients: usize,
    /// Sessions ptyd holds.
    pub sessions: usize,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
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
    /// Health report.
    Doctor(Health),
    /// Screen streams.
    Screens {
        /// Open right now.
        live: Vec<crate::screen::ScreenSummary>,
        /// Closed recently, oldest first, with their final counters.
        closed: Vec<crate::screen::ScreenSummary>,
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
