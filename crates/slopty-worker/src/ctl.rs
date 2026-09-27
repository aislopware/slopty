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
    /// The relay waits on a `PermissionRequest` hook it has just posted as [`Self::Hook`]:
    /// answered with [`CtlReply::Permission`], at once and undecided unless a client follows
    /// the session, and within `wait_ms` whatever happens. The relay keeps its end open while
    /// it waits; closing it withdraws the question.
    Permission(slopty_agent::permission::PermissionAsk),
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
    /// Address ranges whose peers it admits by address, besides loopback and the tailnet.
    pub allow: Vec<String>,
    /// This machine's Tailscale as the daemon reads it; `None` when it can read none, and a
    /// tailnet peer then gets in.
    pub tailscale: Option<Tailscale>,
    /// Clients connected right now.
    pub clients: usize,
    /// Sessions the worker runs, exited ones kept for their last screen included.
    pub sessions: usize,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
}

/// This machine's node, as its Tailscale says.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Tailscale {
    /// `Running` when up; `NeedsLogin`, `Stopped`, … otherwise, or why it did not answer.
    pub state: String,
    /// Its `MagicDNS` name, empty before login.
    pub node: String,
    /// Its tailnet IPv4 address, empty before login.
    pub ip: String,
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
    /// The decision on a [`CtlRequest::Permission`].
    Permission(slopty_agent::permission::PermissionAnswer),
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use slopty_agent::permission::{Decision, PermissionAnswer, PermissionAsk};

    use super::*;

    /// The relay's lines, as `slopty_agent::permission` documents them, are these variants.
    #[test]
    fn a_permission_request_and_its_decision_are_single_json_lines() {
        let session = SessionId::nil();
        let ask = CtlRequest::Permission(PermissionAsk {
            session,
            payload: "{}".to_owned(),
            wait_ms: 1_000,
        });
        let line =
            json!({ "cmd": "permission", "session": session, "payload": "{}", "wait_ms": 1_000 });
        assert_eq!(serde_json::to_value(&ask).ok(), Some(line.clone()));
        assert_eq!(serde_json::from_value::<CtlRequest>(line).ok(), Some(ask));
        let reply = CtlReply::Permission(PermissionAnswer {
            decision: Decision::Deny { message: "no".to_owned(), interrupt: false },
        });
        assert_eq!(
            serde_json::to_value(&reply).ok(),
            Some(json!({
                "reply": "permission",
                "decision": { "kind": "deny", "message": "no", "interrupt": false },
            }))
        );
    }
}
