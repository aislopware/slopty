//! Local control protocol between `slopty-worker` and the `slopty` CLI: newline-delimited JSON
//! over a Unix socket, one request and one reply per connection.

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WorkerId};
use slopty_proto::terminal::SessionSummary;

/// CLI → daemon.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum CtlRequest {
    /// Identity and sessions.
    Status,
    /// Health: permissions, listen address, admitted ranges, connected clients
    /// (`slopty worker doctor`).
    Doctor,
    /// Live screen streams and their worker-side counters (`slopty bench screen` reads the
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

/// What `slopty worker doctor` shows: the daemon's own view of its permissions and links.
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
    /// Where it listens (`[::]:45550` is every interface, both families).
    pub listen: String,
    /// Address ranges whose peers it admits, besides loopback.
    pub allow: Vec<String>,
    /// Clients connected right now.
    pub clients: usize,
    /// Sessions ptyd holds.
    pub sessions: usize,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
}

/// Daemon → CLI.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum CtlReply {
    /// Status.
    Status {
        /// Worker id: the UUID clients key this worker by.
        id: WorkerId,
        /// Worker name.
        name: String,
        /// Sessions.
        sessions: Vec<SessionSummary>,
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
