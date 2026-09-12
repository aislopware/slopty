//! The wire protocol between Slopty clients and hosts.
//!
//! Three transports, one vocabulary:
//!
//! * **Control stream** — one bidirectional QUIC stream. [`ClientMsg`] one way, [`HostMsg`] the
//!   other, each framed by [`codec`] (u32 length prefix + postcard).
//! * **Session streams** — one unidirectional stream per attached terminal, host → client, carrying
//!   [`terminal::TermEvent`]s with the same framing. Input goes back on the control stream so it is
//!   never head-of-line blocked behind a large frame.
//! * **Media datagrams** — unreliable QUIC datagrams with a fixed [`media::MediaHeader`] followed
//!   by a fragment of an encoded video frame, an audio packet, or a cursor update.
//!
//! Compatibility: [`PROTOCOL_VERSION`] is negotiated in `Hello`. Encoded bytes of representative
//! messages are pinned under `src/snapshots`; a changed snapshot is a protocol change.

#![forbid(unsafe_code)]

pub mod agent;
pub mod canvas;
pub mod codec;
pub mod handshake;
pub mod input;
pub mod media;
pub mod screen;
pub mod terminal;

use serde::{Deserialize, Serialize};
use slopty_core::SessionId;

/// Bumped on any incompatible change. Hosts serve exactly one version; clients must match.
pub const PROTOCOL_VERSION: u16 = 25;

/// First message on every host → client session stream, naming the session whose
/// [`terminal::TermEvent`]s follow.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StreamHeader {
    /// The session.
    pub session: SessionId,
}

/// Everything a client sends on the control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on the stream.
    Hello(handshake::Hello),
    /// Terminal request for one session.
    Term {
        /// Target session.
        session: SessionId,
        /// The request.
        req: terminal::TermRequest,
    },
    /// Create a new session; the host answers with `HostMsg::SessionOpened`.
    OpenSession(terminal::OpenSession),
    /// Canvas document operation (host is authoritative; this is a proposal).
    Canvas(canvas::CanvasOp),
    /// Remote window request.
    Screen(screen::ScreenRequest),
    /// Liveness probe; the host echoes it.
    Ping {
        /// Sender's monotonic clock, echoed back untouched.
        sent_at: slopty_core::MonoTime,
    },
    /// Follow or drop an agent session's conversation.
    Transcript(agent::TranscriptFollow),
    /// Register the `slopty hook` relay in the host's Claude Code settings, so agents there
    /// report precisely instead of being guessed at; answered with `HostMsg::HooksInstalled`.
    InstallHooks,
    /// Start an agent the host drives; the host answers with `HostMsg::SessionOpened` of
    /// `SessionKind::Agent`.
    OpenAgent(agent::OpenAgent),
    /// Send the human's words, and the images they attached, to a driven agent as its next
    /// prompt.
    AgentSay {
        /// The agent session.
        session: SessionId,
        /// The prompt.
        text: String,
        /// Pictures that go with it, at most [`agent::IMAGES_MAX`] of at most
        /// [`agent::IMAGE_BYTES_MAX`] each; the host refuses a larger prompt whole.
        images: Vec<agent::Image>,
    },
    /// Answer a driven agent's permission request.
    AgentAnswer(agent::AgentAnswer),
    /// Stop a driven agent's running turn (Esc).
    AgentInterrupt {
        /// The agent session.
        session: SessionId,
    },
    /// Retune a driven agent in place: model, permission mode.
    AgentSet(agent::AgentSet),
    /// List the Claude Code conversations the host has on disk, newest first: those of one
    /// working directory, or of every directory when `None`; answered with
    /// `HostMsg::AgentSessions`.
    ListAgentSessions {
        /// Working directory, or every directory on the host.
        cwd: Option<String>,
    },
}

impl ClientMsg {
    /// Variant name, for logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "Hello",
            Self::Term { .. } => "Term",
            Self::OpenSession(_) => "OpenSession",
            Self::Canvas(_) => "Canvas",
            Self::Screen(_) => "Screen",
            Self::Transcript(_) => "Transcript",
            Self::Ping { .. } => "Ping",
            Self::InstallHooks => "InstallHooks",
            Self::OpenAgent(_) => "OpenAgent",
            Self::AgentSay { .. } => "AgentSay",
            Self::AgentAnswer(_) => "AgentAnswer",
            Self::AgentInterrupt { .. } => "AgentInterrupt",
            Self::AgentSet(_) => "AgentSet",
            Self::ListAgentSessions { .. } => "ListAgentSessions",
        }
    }
}

/// Everything a host sends on the control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum HostMsg {
    /// Reply to `Hello`.
    HelloAck(handshake::HelloAck),
    /// Rejected `Hello`.
    Rejected(handshake::Rejection),
    /// A session now exists (in reply to `OpenSession`, or created by another client).
    SessionOpened(terminal::SessionSummary),
    /// A session is gone.
    SessionClosed {
        /// Which one.
        session: SessionId,
        /// Why.
        reason: terminal::CloseReason,
    },
    /// Out-of-band terminal event that does not belong on the session stream (errors, acks).
    Term {
        /// Session.
        session: SessionId,
        /// Event.
        event: terminal::TermEvent,
    },
    /// Canvas document snapshot or delta.
    Canvas(canvas::CanvasSync),
    /// Remote window event.
    Screen(screen::ScreenEvent),
    /// Agent state change.
    Agent(agent::AgentEvent),
    /// Reply to `Ping`.
    Pong {
        /// The client's timestamp from the ping.
        sent_at: slopty_core::MonoTime,
    },
    /// A slice of an agent session's conversation, for a client following it.
    Transcript(agent::TranscriptUpdate),
    /// Reply to `ClientMsg::InstallHooks`.
    HooksInstalled {
        /// The settings file now registers the relay (whether or not this call changed it).
        ok: bool,
        /// What happened, for the client's notice.
        message: String,
    },
    /// The text a driven agent is streaming right now, so far; empty once it has landed as a
    /// transcript entry.
    AgentPartial {
        /// The agent session.
        session: SessionId,
        /// The message so far.
        text: String,
    },
    /// A driven agent waits on the human for a tool call; answer with `ClientMsg::AgentAnswer`.
    AgentPermission {
        /// The agent session.
        session: SessionId,
        /// The request.
        request: agent::PermissionRequest,
    },
    /// What a driven agent says about itself, whole, whenever any of it changes.
    AgentInfo {
        /// The agent session.
        session: SessionId,
        /// The info.
        info: agent::AgentInfo,
    },
    /// A subagent a driven agent spawned: started, progressing, or done.
    AgentTask {
        /// The agent session.
        session: SessionId,
        /// The subagent, whole.
        task: agent::AgentTask,
    },
    /// The answer to `ClientMsg::ListAgentSessions`.
    AgentSessions {
        /// The working directory listed, or every directory on the host.
        cwd: Option<String>,
        /// Newest first.
        sessions: Vec<agent::AgentSessionInfo>,
    },
}

impl HostMsg {
    /// Variant name, for logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::HelloAck(_) => "HelloAck",
            Self::Rejected(_) => "Rejected",
            Self::SessionOpened(_) => "SessionOpened",
            Self::SessionClosed { .. } => "SessionClosed",
            Self::Term { .. } => "Term",
            Self::Canvas(_) => "Canvas",
            Self::Screen(_) => "Screen",
            Self::Agent(_) => "Agent",
            Self::Transcript(_) => "Transcript",
            Self::Pong { .. } => "Pong",
            Self::HooksInstalled { .. } => "HooksInstalled",
            Self::AgentPartial { .. } => "AgentPartial",
            Self::AgentPermission { .. } => "AgentPermission",
            Self::AgentInfo { .. } => "AgentInfo",
            Self::AgentTask { .. } => "AgentTask",
            Self::AgentSessions { .. } => "AgentSessions",
        }
    }
}
