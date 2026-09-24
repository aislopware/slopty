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
pub mod file;
pub mod handshake;
pub mod input;
pub mod media;
pub mod orchestration;
pub mod screen;
pub mod server;
pub mod terminal;

use serde::{Deserialize, Serialize};
use slopty_core::SessionId;

/// Bumped on any incompatible change. Hosts serve exactly one version; clients must match.
pub const PROTOCOL_VERSION: u16 = 50;

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
    /// Register the `slopty hook` relay in the host's Claude Code settings, so agents there
    /// report precisely instead of being guessed at; answered with `HostMsg::HooksInstalled`.
    InstallHooks,
    /// Read a file on the host for a file card; answered with `HostMsg::File`.
    ReadFile {
        /// Absolute path on the host.
        path: String,
    },
    /// Paths under `root` that `query` matches, for the palette's quick open; answered with
    /// `HostMsg::FoundFiles`.
    FindFiles {
        /// An absolute directory on the host, or `~` for its home.
        root: String,
        /// What was typed.
        query: String,
    },
    /// Where this client's viewport is on the canvas, in canvas units, whenever it settles;
    /// `None` when the canvas is no longer on show. The host fans it out as
    /// `CanvasSync::Presence` so the other clients can draw where each other looks.
    Look {
        /// The viewport, or nothing.
        view: Option<canvas::Rect>,
    },
    /// Point the other clients at one card: the host fans it out as `CanvasSync::Pointed`
    /// and each of them offers a jump to it. Nothing is said about the item itself; a
    /// client that does not know it ignores the pointing.
    Point {
        /// The card.
        item: slopty_core::ItemId,
    },
    /// The files this client's file cards show, the whole set each time it changes: the host
    /// looks at each one every so often and answers with `HostMsg::File` again when one has
    /// changed on disk. Empty when the last card goes.
    WatchFiles {
        /// Absolute paths on the host.
        paths: Vec<String>,
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
            Self::Ping { .. } => "Ping",
            Self::InstallHooks => "InstallHooks",
            Self::Look { .. } => "Look",
            Self::Point { .. } => "Point",
            Self::ReadFile { .. } => "ReadFile",
            Self::FindFiles { .. } => "FindFiles",
            Self::WatchFiles { .. } => "WatchFiles",
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
    /// Reply to `ClientMsg::InstallHooks`.
    HooksInstalled {
        /// The settings file now registers the relay (whether or not this call changed it).
        ok: bool,
        /// What happened, for the client's notice.
        message: String,
    },
    /// The answer to `ClientMsg::ReadFile`.
    File {
        /// The path asked for, as asked.
        path: String,
        /// What was there.
        read: file::FileRead,
    },
    /// The answer to `ClientMsg::FindFiles`.
    FoundFiles {
        /// The root asked.
        root: String,
        /// The query answered, so a stale answer can be told from the current one.
        query: String,
        /// Paths relative to `root`, best first; a directory ends in `/`.
        paths: Vec<String>,
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
            Self::Pong { .. } => "Pong",
            Self::HooksInstalled { .. } => "HooksInstalled",
            Self::File { .. } => "File",
            Self::FoundFiles { .. } => "FoundFiles",
        }
    }
}
