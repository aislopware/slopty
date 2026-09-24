//! The wire protocol between Slopty clients and workers.
//!
//! Three transports, one vocabulary:
//!
//! * **Control stream** — one bidirectional QUIC stream. [`ClientMsg`] one way, [`WorkerMsg`] the
//!   other, each framed by [`codec`] (u32 length prefix + postcard).
//! * **Unidirectional streams** — each opens with a [`transfer::UniHead`]. A session stream (worker
//!   → client, one per attached terminal) carries [`terminal::TermEvent`]s with the same framing;
//!   input goes back on the control stream so it is never head-of-line blocked behind a large
//!   frame. A bulk stream (either way, lower priority) carries a file or a large clipboard
//!   representation as raw bytes.
//! * **Tunnel streams** — client-opened bidirectional streams after the control stream, one per
//!   forwarded TCP connection, opening with [`transfer::TunnelOpen`].
//! * **Media datagrams** — unreliable QUIC datagrams with a fixed [`media::MediaHeader`] followed
//!   by a fragment of an encoded video frame, an audio packet, or a cursor update.
//!
//! Compatibility: [`PROTOCOL_VERSION`] is negotiated in `Hello`. Encoded bytes of representative
//! messages are pinned under `src/snapshots`; a changed snapshot is a protocol change.

#![forbid(unsafe_code)]

pub mod agent;
pub mod codec;
pub mod file;
pub mod handshake;
pub mod input;
pub mod items;
pub mod media;
pub mod orchestration;
pub mod screen;
pub mod server;
pub mod terminal;
pub mod transfer;

use serde::{Deserialize, Serialize};
use slopty_core::SessionId;

/// Bumped on any incompatible change. Workers serve exactly one version; clients must match.
pub const PROTOCOL_VERSION: u16 = 53;

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
    /// Create a new session; the worker answers with `WorkerMsg::SessionOpened`.
    OpenSession(terminal::OpenSession),
    /// Item registry operation (worker is authoritative; this is a proposal).
    Items(items::ItemOp),
    /// Remote window request.
    Screen(screen::ScreenRequest),
    /// Liveness probe; the worker echoes it.
    Ping {
        /// Sender's monotonic clock, echoed back untouched.
        sent_at: slopty_core::MonoTime,
    },
    /// Register the `slopty hook` relay in the worker's Claude Code settings, so agents there
    /// report precisely instead of being guessed at; answered with `WorkerMsg::HooksInstalled`.
    InstallHooks,
    /// Read a file on the worker for a file card; answered with `WorkerMsg::File`.
    ReadFile {
        /// Absolute path on the worker.
        path: String,
    },
    /// Paths under `root` that `query` matches, for the palette's quick open; answered with
    /// `WorkerMsg::FoundFiles`.
    FindFiles {
        /// An absolute directory on the worker, or `~` for its home.
        root: String,
        /// What was typed.
        query: String,
    },
    /// Point the other clients at one item: the worker fans it out as `ItemSync::Pointed`
    /// and each of them offers a jump to it. Nothing is said about the item itself; a
    /// client that does not know it ignores the pointing.
    Point {
        /// The item.
        item: slopty_core::ItemId,
    },
    /// The files this client's file cards show, the whole set each time it changes: the worker
    /// looks at each one every so often and answers with `WorkerMsg::File` again when one has
    /// changed on disk. Empty when the last card goes.
    WatchFiles {
        /// Absolute paths on the worker.
        paths: Vec<String>,
    },
    /// Replace a text file's contents; answered with `WorkerMsg::Written`.
    WriteFile {
        /// Absolute path on the worker.
        path: String,
        /// The whole new text.
        text: String,
        /// The modification time of the version the edit started from; the worker refuses
        /// with a conflict when the file on disk is newer. `None` writes regardless.
        base_modified_ms: Option<u64>,
    },
    /// Clipboard sync.
    Clip(transfer::ClipMsg),
    /// File transfer control.
    Xfer(transfer::XferMsg),
}

impl ClientMsg {
    /// Variant name, for logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "Hello",
            Self::Term { .. } => "Term",
            Self::OpenSession(_) => "OpenSession",
            Self::Items(_) => "Items",
            Self::Screen(_) => "Screen",
            Self::Ping { .. } => "Ping",
            Self::InstallHooks => "InstallHooks",
            Self::Point { .. } => "Point",
            Self::ReadFile { .. } => "ReadFile",
            Self::FindFiles { .. } => "FindFiles",
            Self::WatchFiles { .. } => "WatchFiles",
            Self::WriteFile { .. } => "WriteFile",
            Self::Clip(_) => "Clip",
            Self::Xfer(_) => "Xfer",
        }
    }
}

/// Everything a worker sends on the control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum WorkerMsg {
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
    /// Item registry snapshot, delta or pointing.
    Items(items::ItemSync),
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
    /// The answer to `ClientMsg::WriteFile`.
    Written {
        /// The path written, as asked.
        path: String,
        /// What happened.
        result: file::WriteResult,
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
    /// Clipboard sync.
    Clip(transfer::ClipMsg),
    /// File transfer control.
    Xfer(transfer::XferMsg),
    /// The TCP ports listening in a session's process tree, the whole set each time it changes.
    Ports {
        /// The session.
        session: SessionId,
        /// Listening ports.
        ports: Vec<orchestration::Port>,
    },
}

impl WorkerMsg {
    /// Variant name, for logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::HelloAck(_) => "HelloAck",
            Self::Rejected(_) => "Rejected",
            Self::SessionOpened(_) => "SessionOpened",
            Self::SessionClosed { .. } => "SessionClosed",
            Self::Term { .. } => "Term",
            Self::Items(_) => "Items",
            Self::Screen(_) => "Screen",
            Self::Agent(_) => "Agent",
            Self::Pong { .. } => "Pong",
            Self::HooksInstalled { .. } => "HooksInstalled",
            Self::File { .. } => "File",
            Self::Written { .. } => "Written",
            Self::FoundFiles { .. } => "FoundFiles",
            Self::Clip(_) => "Clip",
            Self::Xfer(_) => "Xfer",
            Self::Ports { .. } => "Ports",
        }
    }
}
