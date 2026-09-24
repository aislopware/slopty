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
use slopty_core::{ClientId, SessionId, WorkerId};

use crate::agent::AgentEvent;
use crate::handshake::ClientKind;
use crate::orchestration::{Outcome, Verb};
use crate::screen::VideoCodec;
use crate::terminal::{CloseReason, SessionSummary};

/// Who is dialling the server.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Role {
    /// A worker registering itself.
    Worker(Registration),
    /// A client app or the CLI.
    Client {
        /// Stable identity of the installation.
        client: ClientId,
        /// What kind.
        kind: ClientKind,
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
    /// The port its client listener takes; clients dial the address the server saw the
    /// worker connect from, with this port.
    pub port: u16,
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

/// A display a worker can stream.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DisplayCap {
    /// Display id (opaque, the worker's own numbering).
    pub id: u32,
    /// Size in points.
    pub w: f32,
    /// Size in points.
    pub h: f32,
    /// Backing scale.
    pub scale: f32,
    /// Refresh rate.
    pub hz: f32,
}

/// An agent installed on a worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstalledAgent {
    /// Which.
    pub kind: crate::agent::AgentKind,
    /// Its version string, as it reports it.
    pub version: String,
}

/// What a worker can do and how it is doing, sent at registration and whenever it changes.
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
    pub displays: Vec<DisplayCap>,
    /// Coding agents installed.
    pub agents: Vec<InstalledAgent>,
    /// Screen capture is permitted (macOS Screen Recording granted).
    pub can_capture: bool,
    /// Input injection is permitted (macOS Accessibility granted).
    pub can_inject: bool,
    /// One-minute load average.
    pub load: f32,
    /// Worker software version.
    pub version: String,
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
    /// Milliseconds since the Unix epoch when the server last heard from it.
    pub last_seen_ms: u64,
}

/// A request id, unique per link and direction.
pub type RequestId = u64;

/// Dialer → server.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ToServer {
    /// First message: protocol and role.
    Hello {
        /// Must equal [`crate::PROTOCOL_VERSION`].
        protocol: u16,
        /// Who.
        role: Role,
    },
    /// A client or agent asks for something.
    Request {
        /// Echoed in the reply.
        id: RequestId,
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
    /// A worker's capabilities changed (permission granted, a display attached, load).
    Caps(WorkerCaps),
    /// A worker opened a terminal.
    SessionOpened(SessionSummary),
    /// A worker's terminal ended.
    SessionClosed {
        /// Which.
        session: SessionId,
        /// Why.
        reason: CloseReason,
    },
    /// A worker's agent changed status.
    Agent(AgentEvent),
}

/// Server → dialer.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum FromServer {
    /// The hello was accepted.
    Welcome {
        /// Server protocol version.
        protocol: u16,
        /// The server's name.
        name: String,
    },
    /// The hello was refused; the connection closes after this.
    Refused(Refusal),
    /// For a worker: do this and [`ToServer::Reply`] with the same id.
    Request {
        /// The server's id.
        id: RequestId,
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
    /// For a client or agent: the whole directory, sent after `Welcome`.
    Directory(Vec<WorkerInfo>),
    /// For a client or agent: one worker appeared or changed.
    Worker(WorkerInfo),
    /// For a client or agent: something happened on a worker.
    Event(Event),
}

/// Why the server refused a hello.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Refusal {
    /// Protocol mismatch.
    ProtocolVersion {
        /// What the server speaks.
        server: u16,
    },
    /// A worker with this id is already connected from elsewhere.
    DuplicateWorker,
}

/// Something that happened on a worker, fanned out to clients and agents.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Event {
    /// A terminal opened.
    SessionOpened {
        /// Where.
        worker: WorkerId,
        /// What.
        summary: SessionSummary,
    },
    /// A terminal ended.
    SessionClosed {
        /// Where.
        worker: WorkerId,
        /// Which.
        session: SessionId,
    },
    /// An agent changed status.
    Agent {
        /// Where.
        worker: WorkerId,
        /// What.
        event: AgentEvent,
    },
}
