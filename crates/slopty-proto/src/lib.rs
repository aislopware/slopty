//! The wire protocol between Slopty clients and workers.
//!
//! Three transports, one vocabulary:
//!
//! * **Control stream** — one bidirectional QUIC stream. Each end opens it with a [`wire::Prefix`]
//!   (magic, wire fingerprint, build), then [`ClientMsg`] one way and [`WorkerMsg`] the other, each
//!   framed by [`codec`] (u32 length prefix + postcard).
//! * **Unidirectional streams** — each opens with a [`transfer::UniHead`]. A session stream (worker
//!   → client, one per attached terminal) carries [`terminal::TermEvent`]s with the same framing;
//!   input goes back on the control stream so it is never head-of-line blocked behind a large
//!   frame. A bulk stream (either way, lower priority) carries a file or a large clipboard
//!   representation as raw bytes. A thread stream (worker → client, one per followed agent thread,
//!   below the session streams) carries [`thread::wire::ThreadFrame`]s.
//! * **Tunnel streams** — client-opened bidirectional streams after the control stream, one per
//!   forwarded TCP connection, opening with [`transfer::TunnelOpen`].
//! * **Datagrams** — unreliable QUIC datagrams, each opening with one [`datagram::Channel`] byte.
//!   Media is a fixed [`media::MediaHeader`] followed by a fragment of an encoded video frame, an
//!   audio packet, or a cursor update. A keystroke's input request and the small frame that answers
//!   it also go once as a datagram each, taken only in order, so a lost packet costs a datagram's
//!   trip rather than QUIC's probe timeout.
//! * **The worker's control socket** — local only, between the worker and the CLI, the hook relay
//!   and the app on the same Mac: one line of JSON each way ([`ctl`]).
//! * **The drag helper's pipes** — local only, between the worker and the drag helper it starts
//!   (`slopty-worker dnd`), framed by [`codec`] ([`dnd`]).
//!
//! Encoded bytes of representative messages are pinned as insta goldens under `tests/snapshots`;
//! a changed golden is a wire change.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod agent;
pub mod codec;
pub mod conversation;
pub mod ctl;
pub mod datagram;
pub mod dnd;
pub mod drag;
pub mod file;
pub mod folder;
pub mod git;
pub mod handoff;
pub mod handshake;
pub mod input;
pub mod items;
pub mod lan;
pub mod media;
pub mod orchestration;
pub mod project;
pub mod ptyd;
pub mod screen;
pub mod search;
pub mod server;
pub mod tailnet;
pub mod terminal;
pub mod thread;
pub mod transfer;
pub mod wire;

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs};

/// A caller's number for one request, echoed in its answer so the caller takes its own answer
/// and not another's. Unique per link and direction.
pub type RequestId = u64;

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
    /// Create a new session. The worker answers this client with `WorkerMsg::SessionOpened`
    /// or `WorkerMsg::Failed` under `request`, and tells every client of the session with
    /// `WorkerMsg::SessionChanged`.
    OpenSession {
        /// Echoed in the answer.
        request: RequestId,
        /// The session wanted.
        spec: terminal::OpenSession,
    },
    /// Item registry operation (worker is authoritative; this is a proposal).
    Items(items::ItemOp),
    /// Remote window request.
    Screen(screen::ScreenRequest),
    /// Liveness probe; the worker echoes it.
    Ping {
        /// Sender's monotonic clock, echoed back untouched.
        sent_at: slopty_core::MonoTime,
    },
    /// Read a file on the worker for a file tile; answered with `WorkerMsg::File`.
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
    /// The files this client's file tiles show, the whole set each time it changes: the worker
    /// follows each one on the kernel's events and answers with `WorkerMsg::File` again when one
    /// has changed on disk. Empty when the last file tile goes.
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
        base_modified_ms: Option<WallMs>,
    },
    /// Clipboard sync.
    Clip(transfer::ClipMsg),
    /// File transfer control.
    Xfer(transfer::XferMsg),
    /// List a directory for a folder tile; answered with `WorkerMsg::Folder`.
    ListFolder {
        /// An absolute directory on the worker, or `~/…` in its home.
        path: String,
    },
    /// Start or stop a text search in the files under a directory; answered with
    /// `WorkerMsg::Search` pages as the worker finds matches.
    Search(search::SearchRequest),
    /// The answer to a `WorkerMsg::Handoff`: taken, offered, refused, or the person done with
    /// an edit.
    Handoff(handoff::HandoffReply),
    /// Which handoffs this client takes: sent right after the hello, ahead of anything the
    /// worker could hand it, and again when that changes (the app's last window closed).
    HandoffCaps(handoff::HandoffCaps),
    /// The folders this client's folder tiles show, the whole set each time it changes: the
    /// worker follows each one on the kernel's events and lists it again, as
    /// `WorkerMsg::Folder`, when an entry is added, removed or renamed. Empty when the last folder
    /// tile goes.
    WatchFolders {
        /// Directories on the worker, as `ListFolder` names them.
        paths: Vec<String>,
    },
    /// Something asked of the worker's agent threads ([`thread`]): the thread table, a
    /// thread to follow from a cursor, an intent.
    Thread(thread::wire::ThreadRequest),
    /// The next page of a folder past [`folder::FOLDER_ENTRIES`] entries; answered with
    /// `WorkerMsg::FolderPage`.
    FolderPage {
        /// The directory, as `ListFolder` named it.
        path: String,
        /// The last entry of the page before.
        after: folder::After,
    },
    /// Make a folder, move or rename an entry, or trash one; answered with `WorkerMsg::FsDone`.
    FsOp {
        /// This client's number for it.
        request: RequestId,
        /// What to do.
        op: folder::FsOp,
    },
    /// Do something in the git repository at `repo` (`git`): its status, a commit, a push, a
    /// pull request. Answered with `WorkerMsg::GitDone`.
    Git {
        /// This client's number for it.
        request: RequestId,
        /// A folder in the repository: absolute, or `~/…`.
        repo: String,
        /// What to do.
        op: git::GitOp,
    },
}

impl ClientMsg {
    /// Variant name, for logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "Hello",
            Self::Term { .. } => "Term",
            Self::OpenSession { .. } => "OpenSession",
            Self::Items(_) => "Items",
            Self::Screen(_) => "Screen",
            Self::Ping { .. } => "Ping",
            Self::ReadFile { .. } => "ReadFile",
            Self::FindFiles { .. } => "FindFiles",
            Self::WatchFiles { .. } => "WatchFiles",
            Self::WriteFile { .. } => "WriteFile",
            Self::Clip(_) => "Clip",
            Self::Xfer(_) => "Xfer",
            Self::ListFolder { .. } => "ListFolder",
            Self::Search(_) => "Search",
            Self::Handoff(_) => "Handoff",
            Self::HandoffCaps(_) => "HandoffCaps",
            Self::WatchFolders { .. } => "WatchFolders",
            Self::Thread(_) => "Thread",
            Self::FolderPage { .. } => "FolderPage",
            Self::FsOp { .. } => "FsOp",
            Self::Git { .. } => "Git",
        }
    }
}

/// Everything a worker sends on the control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum WorkerMsg {
    /// Reply to `Hello`.
    HelloAck(handshake::HelloAck),
    /// The answer to this client's `ClientMsg::OpenSession`: the session it asked for.
    SessionOpened {
        /// The request's number.
        request: RequestId,
        /// The new session.
        summary: terminal::SessionSummary,
    },
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
    /// Reply to `Ping`.
    Pong {
        /// The client's timestamp from the ping.
        sent_at: slopty_core::MonoTime,
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
        /// Something the person should know about how the worker answers from now on, in a
        /// sentence for them, said once: its file watches ran out and it looks at folder times
        /// instead, or the tree is too large to hold whole.
        notice: Option<String>,
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
    /// How this link's packets travel, once the worker's Tailscale has a path and again each
    /// time it changes. Never sent over a link that is not on a tailnet.
    Path(tailnet::LinkPath),
    /// What the worker can do changed since [`handshake::HelloAck::caps`]: a permission
    /// granted or taken, a display attached.
    Caps(server::WorkerCaps),
    /// The answer to `ClientMsg::ListFolder`.
    Folder {
        /// The path asked for, as asked.
        path: String,
        /// What was there.
        listing: folder::Listing,
    },
    /// A session exists and is now as summarised: new (whoever opened it), or changed (its
    /// program exited, its directory, branch or size moved).
    SessionChanged(terminal::SessionSummary),
    /// A request this client numbered failed.
    Failed {
        /// The request's number.
        request: RequestId,
        /// What kind of failure.
        code: orchestration::ErrorCode,
        /// For a person to read.
        message: String,
    },
    /// The worker's one-minute load average moved since [`handshake::HelloAck::load`].
    Load(f32),
    /// A page of a text search this client started, or how it ended.
    Search(search::SearchEvent),
    /// A program in a shell asks this client to open a web page or edit a file, or gives an
    /// edit up; answered with `ClientMsg::Handoff`.
    Handoff(handoff::HandoffEvent),
    /// The pull request and worktree an agent's status line names, the whole of it each time
    /// it changes; both `None` once the agent has neither or is gone.
    AgentBranch(agent::AgentBranch),
    /// The worker's thread table, whole or the rows changed since the client's cursor
    /// (`ClientMsg::Thread`'s `Table`), and each change after it.
    Threads(thread::wire::TableFrame),
    /// How an intent this client sent went (`ClientMsg::Thread`'s `Intent` and `Start`).
    IntentDone(thread::wire::IntentDone),
    /// An agent's past sessions in a folder (`ClientMsg::Thread`'s `Sessions`).
    Sessions(thread::wire::PastSessions),
    /// The answer to `ClientMsg::FolderPage`.
    FolderPage {
        /// The path asked for, as asked.
        path: String,
        /// The entry the page starts after, as asked.
        after: folder::After,
        /// The entries after it.
        listing: folder::Listing,
    },
    /// The answer to `ClientMsg::FsOp`.
    FsDone {
        /// The request's number.
        request: RequestId,
        /// How it went.
        outcome: folder::FsOutcome,
    },
    /// The answer to `ClientMsg::Git`.
    GitDone {
        /// The request's number.
        request: RequestId,
        /// How it went.
        outcome: git::GitOutcome,
    },
    /// What was said in the worker's threads (`ClientMsg::Thread`'s `Search`).
    ThreadHits(thread::wire::ThreadHits),
    /// Which threads wrote a file's lines (`ClientMsg::Thread`'s `Authors`).
    Authors(thread::wire::Authors),
}

impl WorkerMsg {
    /// Variant name, for logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::HelloAck(_) => "HelloAck",
            Self::SessionOpened { .. } => "SessionOpened",
            Self::SessionClosed { .. } => "SessionClosed",
            Self::Term { .. } => "Term",
            Self::Items(_) => "Items",
            Self::Screen(_) => "Screen",
            Self::Pong { .. } => "Pong",
            Self::File { .. } => "File",
            Self::Written { .. } => "Written",
            Self::FoundFiles { .. } => "FoundFiles",
            Self::Clip(_) => "Clip",
            Self::Xfer(_) => "Xfer",
            Self::Ports { .. } => "Ports",
            Self::Path(_) => "Path",
            Self::Caps(_) => "Caps",
            Self::Folder { .. } => "Folder",
            Self::SessionChanged(_) => "SessionChanged",
            Self::Failed { .. } => "Failed",
            Self::Load(_) => "Load",
            Self::Search(_) => "Search",
            Self::Handoff(_) => "Handoff",
            Self::AgentBranch(_) => "AgentBranch",
            Self::Threads(_) => "Threads",
            Self::IntentDone(_) => "IntentDone",
            Self::Sessions(_) => "Sessions",
            Self::FolderPage { .. } => "FolderPage",
            Self::FsDone { .. } => "FsDone",
            Self::GitDone { .. } => "GitDone",
            Self::ThreadHits(_) => "ThreadHits",
            Self::Authors(_) => "Authors",
        }
    }
}
