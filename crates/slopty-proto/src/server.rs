//! Links to the server: the control plane (`docs/decisions/topology.md`).
//!
//! The server is never on the data path. Workers dial it and keep one QUIC connection whose
//! control stream is both their lease and the channel the server sends [`Verb`]s down.
//! Clients, the CLI and agents dial it for the worker directory and to send verbs. Terminal
//! rows and video still go client ↔ worker directly, on the protocol in the crate root.
//!
//! Every link opens one bidirectional stream. The dialer's first message is [`ToServer::Hello`]
//! naming its role; the server answers [`FromServer::Welcome`] or [`FromServer::Refused`]. After
//! that each side sends its role's messages, framed by [`crate::codec`] like the rest.

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId};

use crate::RequestId;
use crate::agent::AgentEvent;
use crate::orchestration::{HubEvent, IdempotencyKey, Outcome, Verb};
use crate::screen::{DisplayInfo, VideoCodec};
use crate::terminal::{CloseReason, SessionSummary};

/// Who is dialling the server.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Role {
    /// A worker registering itself; boxed, being ten times the size of the other roles.
    Worker(Box<Registration>),
    /// A client app or the CLI.
    Client {
        /// Its name ("Cong's iPad").
        name: String,
    },
    /// An automation surface acting for an AI agent (`slopty mcp`, the MCP endpoint).
    Agent {
        /// What it calls itself, for logs.
        name: String,
    },
}

/// What a worker says about itself when it registers.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Registration {
    /// Its identity, minted once and kept in its data directory.
    pub worker: WorkerId,
    /// Its name ("mac-studio").
    pub name: String,
    /// Where its client listener is bound: every interface (`[::]`), or the one address
    /// `--bind` gave it. The server publishes the port at an address clients can reach
    /// (`slopty_server::link`).
    pub listen: std::net::SocketAddr,
    /// What it can do.
    pub caps: WorkerCaps,
    /// The terminals it runs now.
    pub sessions: Vec<SessionSummary>,
}

/// Operating system of a worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Os {
    /// macOS.
    MacOs,
    /// Linux.
    Linux,
}

/// An agent installed on a worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstalledAgent {
    /// Which.
    pub kind: crate::agent::AgentKind,
    /// Its version string, as it reports it.
    pub version: String,
}

/// What a worker can do, sent at registration and whenever it changes.
///
/// How loaded it is moves all the time and travels on its own ([`ToServer::Load`],
/// [`crate::WorkerMsg::Load`]), so two of these compare equal exactly when the worker can do
/// the same things.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct WorkerCaps {
    /// Operating system.
    pub os: Os,
    /// OS version string.
    pub os_version: String,
    /// CPU architecture (`aarch64`, `x86_64`).
    pub arch: String,
    /// Logical CPUs.
    pub cpus: u16,
    /// Physical memory, bytes.
    pub memory: u64,
    /// Hardware video encoders.
    pub encoders: Vec<VideoCodec>,
    /// Displays.
    pub displays: Vec<DisplayInfo>,
    /// Coding agents installed.
    pub agents: Vec<InstalledAgent>,
    /// Screen capture is permitted (macOS Screen Recording granted).
    pub can_capture: bool,
    /// Input injection is permitted (macOS Accessibility granted).
    pub can_inject: bool,
    /// Worker software version.
    pub version: String,
}

impl WorkerCaps {
    /// A worker on `os` that says nothing else about itself: no permission, no display, no
    /// encoder, no agent.
    #[must_use]
    pub const fn bare(os: Os) -> Self {
        Self {
            os,
            os_version: String::new(),
            arch: String::new(),
            cpus: 0,
            memory: 0,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: false,
            can_inject: false,
            version: String::new(),
        }
    }
}

/// Whether the server can reach a worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Liveness {
    /// Connected.
    Online,
    /// The connection went quiet; it may come back.
    Unreachable,
    /// Quiet long enough that it is presumed gone; its terminals show stale.
    Gone,
}

/// A worker as the directory lists it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct WorkerInfo {
    /// Identity.
    pub worker: WorkerId,
    /// Name.
    pub name: String,
    /// Where clients dial it (`ip:port`), from the address it registered from.
    pub address: String,
    /// Reachability.
    pub liveness: Liveness,
    /// Capabilities as last reported.
    pub caps: WorkerCaps,
    /// Its one-minute load average as last reported.
    pub load: f32,
    /// When the server last heard from it.
    pub last_seen_ms: WallMs,
}

/// Dialer → server.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ToServer {
    /// First message: who is dialing.
    Hello {
        /// Who.
        role: Role,
    },
    /// A client or agent asks for something.
    Request {
        /// Echoed in the reply.
        id: RequestId,
        /// The caller's name for the verb's effect, so a repeat does not do it twice.
        key: Option<IdempotencyKey>,
        /// What.
        verb: Verb,
    },
    /// A worker answers a request the server forwarded.
    Reply {
        /// The server's id for it.
        id: RequestId,
        /// The answer.
        outcome: Outcome,
    },
    /// A worker's capabilities changed (permission granted, a display attached).
    Caps(WorkerCaps),
    /// A worker's terminal is new or changed (its program exited, its directory moved).
    SessionChanged(SessionSummary),
    /// A worker's terminal ended.
    SessionClosed {
        /// Which.
        session: SessionId,
        /// Why.
        reason: CloseReason,
    },
    /// A worker's agent changed status.
    Agent(AgentEvent),
    /// A worker's one-minute load average moved.
    Load(f32),
}

/// Server → dialer.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum FromServer {
    /// The hello was accepted.
    Welcome {
        /// The server's name.
        name: String,
    },
    /// The hello was refused; the connection closes after this.
    Refused(Refusal),
    /// For a worker: do this and [`ToServer::Reply`] with the same id.
    Request {
        /// The server's id.
        id: RequestId,
        /// The caller's key, passed on: the worker does the verb once per key.
        key: Option<IdempotencyKey>,
        /// What.
        verb: Verb,
    },
    /// For a client or agent: the answer to its request.
    Reply {
        /// Its id.
        id: RequestId,
        /// The answer.
        outcome: Outcome,
    },
    /// For a client or agent: the whole directory, sent after `Welcome` and again after the
    /// link fell behind.
    Directory(Vec<WorkerInfo>),
    /// For a client or agent: one worker appeared or changed.
    Worker(WorkerInfo),
    /// For a client or agent: something happened on a worker, numbered in the log
    /// [`Verb::Events`] reads.
    Event(HubEvent),
    /// For a client or agent: every terminal on every worker, each with its agent, sent after
    /// the directory. What the client showed of terminals and agents before is replaced.
    Terminals(Vec<(WorkerId, SessionSummary)>),
    /// For a client or agent: a worker's load moved. Its own message, so a tick of it resends
    /// nothing else and changes nothing that is saved.
    Load {
        /// Which.
        worker: WorkerId,
        /// Its 1-minute load average.
        load: f32,
    },
}

/// Why the server refused a hello.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Refusal {
    /// A worker with this id is already connected from elsewhere.
    DuplicateWorker,
    /// The tailnet grants this node no such role here: it belongs to another user, or is
    /// tagged, and no grant names the role.
    NotGranted,
}

impl Refusal {
    /// The refusal as a person reads it, in a sentence of its own.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NotGranted => "Not granted by the tailnet policy",
            Self::DuplicateWorker => "A worker with this id is already connected",
        }
    }
}
