// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! The entities the view model serves the shell.
//!
//! Devices, projects (spaces), chats and their sessions, the links to the server, and the
//! requests and answers of a machine's files, git, project actions and terminals.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{HarnessId, ReasoningLevel, SandboxLevel};

/// A machine Slopty reaches: this one, or another the person works on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Its id.
    pub id: String,
    /// Its name, as the person sees it.
    pub name: String,
    /// Its operating system.
    pub platform: String,
    /// When it was last heard from.
    pub last_seen_at: Option<DateTime<Utc>>,
    /// First registration time, for the Devices page's "Added …" fragment.
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// App version the device's engine last booted with — fleet staleness at a
    /// glance (Devices page). Optional so pre-existing docs stay readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What the person can do to the machine from here (update, remove, wake…), as the
    /// app offers it; the Devices page shows each and calls `DeviceAction` with its id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<DeviceAction>,
    /// Where this client reaches the machine (its tailnet name or address), for the
    /// person's own tools: an editor's SSH remote. None for this machine's own worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    /// Why the machine turns this device away, in words for its badge's tooltip (its
    /// tailnet policy grants this device no role there, or its allow ranges do not hold this
    /// device's address); None while it lets this device in. The badge retries with the row's
    /// `DeviceAction::REDIAL` action and never asks for a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turned_away: Option<String>,
}

/// One thing the person can do to a machine or the server, named by the app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAction {
    /// What the app is asked to do, handed back in `DeviceAction`.
    pub id: String,
    /// The button's words.
    pub label: String,
}

impl DeviceAction {
    /// The id of what a machine that turns this device away offers ([`Device::turned_away`]),
    /// its badge's click: dial it again now rather than when its link would next try.
    pub const REDIAL: &'static str = "redial";
}

/// A (device, folder) pair: the unit of organization in the sidebar.
///
/// Sessions belong to exactly one space; the space fixes their host device and base cwd.
/// Folders need not be git repos: `git_detected` is stamped by the owning device and gates
/// branch pickers and the diff sidebar on every device without an RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Space {
    /// Its id.
    pub id: String,
    /// Owning device — fixed at create, immutable.
    pub device_id: String,
    /// Absolute folder path on the owning device.
    pub path: String,
    /// User rename; absent ⇒ display = basename(path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Owner-stamped: is `path` inside a git work tree?
    #[serde(default)]
    pub git_detected: bool,
    /// Owner-stamped freshness timestamp of the last git check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_checked_at: Option<DateTime<Utc>>,
    /// Owner-stamped when git: canonical checkout identity of the space root
    /// (`sha256(deviceId ‖ NUL ‖ git_dir)`) — diff grouping key for root sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    /// Owner-stamped when git: identity shared by every clone and worktree of
    /// one repository — its trunk's root commit (`commit:<sha>`, which
    /// survives renames, transfers and remote spellings), else for a shallow
    /// or empty history the normalized origin remote (`host/owner/repo`),
    /// else `local:` + a device-scoped hash of the common git dir. Opaque to
    /// readers: projects with equal ids group together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    /// When it was made.
    pub created_at: DateTime<Utc>,
}

impl Space {
    /// Name override, else basename(path), else the path itself.
    /// Lives here (proto) so UI and engine agree on the derivation.
    pub fn display_name(&self) -> &str {
        if let Some(name) = self.name.as_deref()
            && !name.trim().is_empty()
        {
            return name;
        }
        let trimmed = self.path.trim_end_matches(['/', '\\']);
        trimmed.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(&self.path)
    }
}

/// What a chat's agent runs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConfig {
    /// The agent.
    pub harness: HarnessId,
    /// The model, when one was chosen.
    pub model: Option<String>,
    /// The effort, when one was chosen.
    pub reasoning: Option<ReasoningLevel>,
    /// The model's other settings, by id.
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    /// What its commands may touch.
    pub sandbox: SandboxLevel,
}

/// Immutable-at-run-start repository context owned by one conversation.
///
/// This is deliberately separate from the live checkout snapshot: another
/// chat may change the branch at the same checkout without changing which
/// branch this conversation belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSourceContext {
    /// The checkout's id.
    pub checkout_id: String,
    /// The repository's root folder.
    pub repo_root: String,
    /// The folder the agent ran in.
    pub cwd: String,
    /// The branch it ran on.
    pub branch: String,
    /// The commit checked out, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// When this was read.
    pub observed_at: DateTime<Utc>,
}

/// A conversation with an agent, as the sidebar lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    /// Its id.
    pub id: String,
    /// Owning (host) device.
    pub device_id: String,
    /// Its title, once it has one.
    pub title: Option<String>,
    /// Whether the person archived it.
    pub archived: bool,
    /// The folder its agent runs in.
    pub cwd: Option<String>,
    /// The branch it runs on, when in a repository.
    pub branch: Option<String>,
    /// Canonical id of the repo checkout/worktree this chat operates in.
    pub checkout_id: Option<String>,
    /// Repository identity captured for this conversation immediately before
    /// its harness run. Unlike `branch`, this is never inferred from another
    /// chat sharing the same checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_context: Option<ConversationSourceContext>,
    /// What its agent runs with.
    pub config: Option<ChatConfig>,
    /// Its last message, cut to a line.
    pub last_message_preview: Option<String>,
    /// When its last message came.
    pub last_message_at: Option<DateTime<Utc>>,
    /// When it was made.
    pub created_at: DateTime<Utc>,
    /// Harness-native session id of the chat's latest run — engine-owned resume
    /// continuity across engine restarts.
    /// Empty string = explicit
    /// "do not resume" tombstone after a rejected resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_session_id: Option<String>,
    /// Cwd the harness session was created under. Harness session stores are
    /// cwd-scoped (claude keys conversations by project directory), so resume
    /// is only injected when the next run launches from the same cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_session_cwd: Option<String>,
    /// The space this chat belongs to. Invariant: `Some` for every UI-created
    /// chat; rows with a missing/dangling space id are not rendered (the host
    /// device's repair sweep deletes its own danglers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// Synced LWW seen marker — compared against `last_message_at` to derive
    /// the "completed (finished but unseen)" indicator. Reading a chat on any
    /// device clears the badge everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    /// The chat this one hangs off: the conversation a side chat was forked
    /// from, or the chat whose agent spawned this one through the Slopty MCP
    /// server. Children stay out of the main sidebar: a subagent lives in its
    /// parent's chat, a side chat in the explorer. Absent for top-level chats;
    /// a dangling id (parent deleted) is tolerated rather than cascaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_chat_id: Option<String>,
    // A subagent lives in its parent's chat (its inline card, the Background tray, a
    // drill-in; `docs/decisions/ui.md`, "Subagents live in their parent's chat"); a side chat in
    // the explorer.
    /// A subagent its parent's agent started, not a side chat the person opened.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subagent: bool,
    // A call's subagent runs inside its agent's session, and shows that agent's panes.
    /// A subagent of one of its parent's calls, running inside the parent's agent (its
    /// terminal, its checkout), not an agent started on its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_parent: bool,
    /// What the person was writing to it and has not sent, as a device last kept it on
    /// its machine; the composer restores it on open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
}

impl Chat {
    /// A session the user sees in chat lists: not another chat's worker
    /// (`parent_chat_id`).
    pub const fn is_top_level(&self) -> bool {
        self.parent_chat_id.is_none()
    }
}

impl Chat {
    /// True when the chat has activity the user hasn't seen on any device.
    pub fn unseen(&self) -> bool {
        match (self.last_message_at, self.last_seen_at) {
            (Some(msg), Some(seen)) => msg > seen,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

/// Display status for a chat row/tab: the four user-facing states plus a
/// distinct Errored. Derived — never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatIndicator {
    /// The agent works.
    Working,
    /// The agent waits on the person.
    AwaitingInput,
    /// The agent failed, and the person has not seen it.
    Errored,
    /// Finished running (or errored out) but not seen yet on any device.
    Completed,
    /// Nothing to say.
    Idle,
}

/// Derive the display status. `live` must already be staleness-gated by the
/// caller (the UI's 45s window) — pass `None` for a stale/absent session row.
pub fn chat_indicator(chat: &Chat, live: Option<&Session>) -> ChatIndicator {
    match live.map(|s| s.status) {
        Some(SessionStatus::Working) => ChatIndicator::Working,
        Some(SessionStatus::AwaitingInput) => ChatIndicator::AwaitingInput,
        Some(SessionStatus::Errored) if chat.unseen() => ChatIndicator::Errored,
        _ if chat.unseen() => ChatIndicator::Completed,
        _ => ChatIndicator::Idle,
    }
}

/// Where a session's agent stands, as its worker reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    /// At rest.
    Idle,
    /// It works.
    Working,
    /// It waits on the person.
    AwaitingInput,
    /// It failed.
    Errored,
    // An agent's phases stay apart rather than fold into Idle.
    /// At its prompt, waiting on its own background work or a wakeup, not on the person:
    /// nothing to stop, and it works again when what it waits on wakes it.
    Waiting,
    /// It finished what it was asked.
    Done,
    /// The person stopped it.
    Stopped,
}

// What a session waits on, for the sidebar's reason line.
/// What a session waits on: while it awaits the person, the request it holds open; while it
/// works, a wait of its own such as `compacting`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWait {
    /// Open: `question` (the agent asks), `plan` (a plan to approve), `approval` (a call to
    /// allow), `compacting` (its context is being summarised); any other awaits input.
    pub kind: String,
    /// The request's tool in a line ("run cargo test"), control and bidi characters taken
    /// out, at most 60 graphemes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl SessionWait {
    /// A call waits to be allowed.
    pub const APPROVAL: &'static str = "approval";
    /// The agent's context is being summarised.
    pub const COMPACTING: &'static str = "compacting";
    /// A plan waits for approval.
    pub const PLAN: &'static str = "plan";
    /// The agent asks the person questions.
    pub const QUESTION: &'static str = "question";
}

/// Live run status for a chat — drives the Working indicator and sidebar status dots.
/// Staleness-checked client-side against `updated_at` so a crashed backend never shows
/// an eternal "Working".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Last successfully completed assistant turn. Retained while the next turn
    /// runs so coalesced status watches do not lose normal queue completions.
    /// Interrupts, failures and liveness expiry never advance this marker.
    #[serde(default)]
    pub last_completed_turn: Option<String>,
    /// The chat it runs.
    pub chat_id: String,
    /// The machine it runs on.
    pub device_id: String,
    /// Where it stands.
    pub status: SessionStatus,
    /// When it started, when known.
    pub started_at: Option<DateTime<Utc>>,
    /// When its worker last reported it.
    pub updated_at: DateTime<Utc>,
    /// Subagents of this chat streaming right now. Rides the session row (and
    /// its staleness window) so every device's sidebar can badge a chat it
    /// has not opened; read it through `view::running_subagents`.
    #[serde(default)]
    pub running_subagents: u32,
    // Since when its status stands, on its machine's clock, and what it waits on.
    /// When the status last changed, as the worker says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_since: Option<DateTime<Utc>>,
    /// What it waits on, while it waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<SessionWait>,
}

/// One entry of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    /// Its name.
    pub name: String,
    /// Whether it is a folder.
    pub is_dir: bool,
    /// Whether it is a repository's root.
    pub is_repo: bool,
}

/// A folder's entries on a machine, for the folder picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderListing {
    /// The folder.
    pub path: String,
    /// Its entries.
    pub entries: Vec<FolderEntry>,
    /// True when the listing hit the entry cap.
    #[serde(default)]
    pub truncated: bool,
}

/// `SearchHomeFolders`: folders anywhere under the host's home whose names
/// match `query`, for the new-project picker. An empty query only starts the
/// host's home search index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHomeFoldersRequest {
    /// The words to match.
    pub query: String,
    /// The most matches wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
}

/// A folder that matched a home folder search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeFolderMatch {
    /// Absolute path on the host.
    pub path: String,
    /// Home-relative, `/`-separated (what the picker shows).
    pub relative: String,
    /// Whether it is a repository's root.
    pub is_repo: bool,
}

/// What a home folder search found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeFolderSearchResult {
    /// The folders, best first.
    pub matches: Vec<HomeFolderMatch>,
    /// The host's home index was still scanning; results may be incomplete.
    pub indexing: bool,
}

/// A browse root beyond home: a mounted drive/volume (or the system root).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveEntry {
    /// Display name (volume label / mount folder name; "System" for `/`).
    pub name: String,
    /// Absolute mount point.
    pub path: String,
}

/// The browse roots beyond home a machine has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveListing {
    /// Its drives.
    pub drives: Vec<DriveEntry>,
}

/// Identifies the local checkout used by workspace file operations.
/// Exactly one of `chat_id` and `space_id` must be present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTarget {
    /// The chat whose checkout it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// The project whose folder it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// A folder inside it to use instead of its root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_path: Option<String>,
}

/// `ListWorkspaceDirectory`: one page of a workspace folder's entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListWorkspaceDirectoryRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The folder, workspace-relative; empty for the root.
    #[serde(default)]
    pub directory: String,
    /// Whether to list what git ignores.
    #[serde(default)]
    pub include_ignored: bool,
    /// Where the page starts: the last page's `next_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// One page of a workspace folder's entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDirectoryPage {
    /// The checkout's id, when it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    /// What the workspace lets the person change, when it lets them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_capabilities: Option<WorkspaceMutationCapabilities>,
    /// The folder.
    pub directory: String,
    /// Its entries.
    pub entries: Vec<WorkspaceEntry>,
    /// Where the next page starts, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Whether the folder held more than any page lists.
    pub truncated: bool,
}

/// One entry of a workspace folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    /// The revision a change to it must name, when it can be changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_revision: Option<String>,
    /// Its path, workspace-relative.
    pub path: String,
    /// Its name.
    pub name: String,
    /// What it is.
    pub kind: WorkspaceEntryKind,
    /// Its size in bytes, for a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// When it last changed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
    /// Whether git ignores it.
    pub ignored: bool,
    /// Whether it cannot be written.
    pub read_only: bool,
}

/// What a workspace entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceEntryKind {
    /// A file.
    File,
    /// A folder.
    Directory,
    /// A symbolic link.
    Symlink,
}

/// `SearchWorkspaceFiles`: files of a workspace whose paths match a query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWorkspaceFilesRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The words to match.
    pub query: String,
    /// Whether to search what git ignores.
    #[serde(default)]
    pub include_ignored: bool,
    /// The most matches wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
}

/// A file that matched a path search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileSearchMatch {
    /// Its path, workspace-relative.
    pub path: String,
    /// Its name.
    pub name: String,
    /// What it is.
    pub kind: WorkspaceEntryKind,
    /// How well it matched; higher is better.
    pub score: i64,
}

/// Plain-text search over a workspace's file contents (smart case). Binary
/// and oversized files are skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWorkspaceContentRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The text to find.
    pub query: String,
    /// The most matches wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    /// The most matches wanted from one file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_file_limit: Option<u16>,
}

/// A line that matched a content search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceContentMatch {
    /// Workspace-relative, `/`-separated.
    pub path: String,
    /// 1-based.
    pub line: u64,
    /// 0-based byte column of the first match in the original line.
    pub column: u64,
    /// The matched line, leading whitespace trimmed, at most 512 bytes.
    pub preview: String,
    /// `[start, end)` byte ranges of the matches within `preview`.
    pub ranges: Vec<(u32, u32)>,
}

/// What a content search found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceContentSearchResult {
    /// The matching lines.
    pub matches: Vec<WorkspaceContentMatch>,
    /// More matches exist, or the host's time budget ran out.
    pub truncated: bool,
    /// The host's index was still scanning; results may be incomplete.
    pub indexing: bool,
}

/// Start (or keep alive) the host's search index for a workspace, so the
/// chat's first search finds it built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmWorkspaceSearchRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
}

/// Where a workspace's search index stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceSearchIndexState {
    /// The initial scan is still running; searches may be incomplete.
    Building,
    /// Built: searches are complete.
    Ready,
}

/// Where the index stands after `WarmWorkspaceSearch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmWorkspaceSearchResult {
    /// Where it stands.
    pub state: WorkspaceSearchIndexState,
}

/// `ReadWorkspaceFile`: a workspace file's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWorkspaceFileRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The file, workspace-relative.
    pub path: String,
}

/// Workspace images travel in bounded relay frames, independently of text reads.
pub const MAX_WORKSPACE_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// The most bytes of an image one chunk carries.
pub const WORKSPACE_IMAGE_CHUNK_BYTES: usize = 384 * 1024;

/// `ReadWorkspaceImage`: one chunk of a workspace image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWorkspaceImageRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The image, workspace-relative.
    pub path: String,
    /// The checkout the image must still be in.
    pub expected_checkout_id: String,
    /// Where the chunk starts, in bytes.
    pub offset: usize,
    /// The image's hash from the first chunk, so a later chunk is of the same image.
    pub expected_content_hash: Option<String>,
}

/// One chunk of a workspace image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceImageChunk {
    /// The checkout it was read from.
    pub checkout_id: String,
    /// The whole image's hash.
    pub content_hash: String,
    /// Its media type.
    pub mime_type: String,
    /// The chunk's bytes, in base64.
    pub data: String,
    /// Where the next chunk starts.
    pub next_offset: usize,
    /// The whole image's size in bytes.
    pub size: usize,
    /// Whether this is the last chunk.
    pub done: bool,
}

/// A workspace file's text, as read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileText {
    /// Identity of the checkout this snapshot was read from.
    pub checkout_id: String,
    /// The file, workspace-relative.
    pub path: String,
    /// Its text; none for a file that is not text or is too large.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Its content's hash, which a write must name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Its size in bytes.
    pub size: u64,
    /// When it last changed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
    /// How its text is encoded.
    pub encoding: WorkspaceTextEncoding,
    /// Its line endings, for text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_ending: Option<WorkspaceLineEnding>,
    /// Why it cannot be written here, when it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_reason: Option<WorkspaceReadOnlyReason>,
    /// Whether `text` stops short of the file's end.
    pub truncated: bool,
}

/// How a file's text is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceTextEncoding {
    /// UTF-8.
    Utf8,
    /// UTF-8 after a byte order mark.
    Utf8Bom,
    /// Not text.
    Binary,
    /// Text in an encoding Slopty does not read.
    Unsupported,
}

/// A text file's line endings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceLineEnding {
    /// `
    /// `.
    Lf,
    /// `\r
    /// `.
    Crlf,
    /// Both.
    Mixed,
    /// No line break at all.
    None,
}

/// Why a workspace file cannot be written here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceReadOnlyReason {
    /// It is not text.
    Binary,
    /// Its encoding is one Slopty does not write.
    UnsupportedEncoding,
    /// Its lines end two ways.
    MixedLineEndings,
    /// It is a symbolic link.
    Symlink,
    /// It is larger than an editor opens.
    TooLarge,
    /// The machine does not let it be written.
    PermissionDenied,
    /// It is not a regular file.
    NotRegularFile,
    /// A file read by absolute path beyond the chat's workspace root: reads
    /// are allowed, writes never are. Older peers never see this variant —
    /// only a new UI asks for an absolute path — so the wire stays additive.
    OutsideWorkspace,
}

/// `WriteWorkspaceFile`: a workspace file's new text, written only over the text it was read as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteWorkspaceFileRequest {
    /// Must match the read snapshot, even if the chat has since changed cwd.
    pub expected_checkout_id: String,
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The file, workspace-relative.
    pub path: String,
    /// The new text.
    pub text: String,
    /// The hash the file must still have.
    pub expected_content_hash: String,
    /// How to encode the text.
    pub encoding: WorkspaceWritableEncoding,
    /// Which line endings to write.
    pub line_ending: WorkspaceWritableLineEnding,
}

/// An encoding Slopty writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceWritableEncoding {
    /// UTF-8.
    Utf8,
    /// UTF-8 after a byte order mark.
    Utf8Bom,
}

/// Line endings Slopty writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceWritableLineEnding {
    /// `
    /// `.
    Lf,
    /// `\r
    /// `.
    Crlf,
}

/// What came of a write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WriteWorkspaceFileOutcome {
    /// The file was written.
    #[serde(rename_all = "camelCase")]
    Written {
        /// The file as written.
        file: WorkspaceFileWriteResult,
    },
    /// The file changed since it was read, and nothing was written.
    #[serde(rename_all = "camelCase")]
    Conflict {
        /// How it changed.
        reason: WorkspaceFileConflictReason,
        /// Its hash now, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_content_hash: Option<String>,
        /// When it changed, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_modified_at: Option<DateTime<Utc>>,
    },
}

/// A file as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileWriteResult {
    /// The file, workspace-relative.
    pub path: String,
    /// Its content's hash now.
    pub content_hash: String,
    /// Its size in bytes.
    pub size: u64,
    /// When it was written, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
}

/// How a file changed under a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceFileConflictReason {
    /// Its text changed.
    Changed,
    /// It was deleted.
    Deleted,
    /// Another file took its place.
    Replaced,
    /// It is no longer a regular file.
    NotRegularFile,
}

/// `WatchWorkspaceFiles`: a workspace's file changes as they happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchWorkspaceFilesRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
}

/// One batch of a workspace's file changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileChanges {
    /// The batch's place in the stream.
    pub sequence: u64,
    /// Changes were missed: read the workspace again.
    pub resync_required: bool,
    /// The changes.
    pub changes: Vec<WorkspaceFileChange>,
}

/// One change to a workspace's files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileChange {
    /// The change's id, when Slopty made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// What changed.
    pub kind: WorkspaceFileChangeKind,
    /// The path changed, workspace-relative.
    pub path: String,
    /// Where it was, for a rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

/// What changed about a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceFileChangeKind {
    /// It appeared.
    Created,
    /// Its content changed.
    Modified,
    /// It went away.
    Removed,
    /// It moved.
    Renamed,
}

/// One file of a diff, in a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFileSummary {
    /// The file.
    pub path: String,
    /// Previous path for renames/copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// What happened to it, as git says.
    pub status: String,
    /// Lines added.
    pub additions: u32,
    /// Lines deleted.
    pub deletions: u32,
    /// Whether it is not text.
    #[serde(default)]
    pub binary: bool,
}

/// Git porcelain states, independent of patch size and line counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitFileState {
    /// Unchanged.
    Unchanged,
    /// Added.
    Added,
    /// Modified.
    Modified,
    /// Deleted.
    Deleted,
    /// Renamed.
    Renamed,
    /// Copied.
    Copied,
    /// Conflicted in a merge.
    Unmerged,
    /// Not tracked.
    Untracked,
    /// Its type changed (a file became a link).
    TypeChanged,
}

/// One file's git state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileStatus {
    /// The file.
    pub path: String,
    /// Where it was, for a rename or copy.
    pub old_path: Option<String>,
    /// Its state in the index.
    pub index: GitFileState,
    /// Its state in the working tree.
    pub worktree: GitFileState,
}

/// Latest status only: never contains file content or a patch. `complete = false`
/// means unavailable/partial, not clean. Revision covers only these statuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutGitStatus {
    /// The checkout.
    pub checkout_id: String,
    /// The machine it is on.
    pub device_id: String,
    /// The statuses' revision.
    pub revision: String,
    /// Whether every file was read.
    pub complete: bool,
    /// The files that are not unchanged.
    pub files: Vec<GitFileStatus>,
}

/// Keep unavailable updates inside an object: the RPC envelope uses JSON null
/// for a missing item, so a bare optional snapshot cannot signal invalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitStatusFrame {
    /// The status, none while it cannot be read.
    pub status: Option<CheckoutGitStatus>,
}

/// Working-tree diff for a checkout — latest-only sidecar, 3 MiB patch cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutDiff {
    /// The checkout.
    pub checkout_id: String,
    /// The machine it is on.
    pub device_id: String,
    /// The folder it was read from.
    pub cwd: String,
    /// The patch.
    pub patch: String,
    /// Its files, in a line each.
    pub files: Vec<DiffFileSummary>,
    /// Lines added.
    pub additions: u32,
    /// Lines deleted.
    pub deletions: u32,
    /// True when the patch was truncated at the byte cap ("Partial snapshot").
    pub truncated: bool,
    /// The patch's checksum, which a file's diff text names.
    pub checksum: String,
    /// When it was read.
    pub updated_at: DateTime<Utc>,
}

/// `GetCheckoutFileDiffText`: both sides of one file of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCheckoutFileDiffTextRequest {
    /// The checkout.
    pub checkout_id: String,
    /// The folder it is read from.
    pub cwd: String,
    /// The file.
    pub path: String,
    /// Which diff: the working tree's, or the branch's against its base.
    #[serde(default)]
    pub mode: String,
    /// The base a branch diff is against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// The chat whose checkout it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// Pinned commit for History's per-commit diff scope. When present, the
    /// source pair is read from the commit parent and this commit, never from
    /// the live working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    /// The checksum of the diff the file is from.
    pub diff_checksum: String,
}

/// Both sides of one file of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutFileDiffText {
    /// The checksum of the diff it is from.
    pub diff_checksum: String,
    /// The text before, none for a new file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    /// The text after, none for a deleted file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
    /// The hash of the text before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_content_hash: Option<String>,
    /// The hash of the text after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_content_hash: Option<String>,
    /// Whether it is not text.
    pub binary: bool,
    /// Whether either side stops short.
    pub truncated: bool,
    /// Whether the diff changed since its checksum was read.
    #[serde(default)]
    pub stale: bool,
}

/// Who is signed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    /// Their id.
    pub id: String,
    /// Their email address.
    pub email: String,
    /// Their name, when they gave one.
    pub name: Option<String>,
}

/// Whether anyone is signed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum AuthState {
    /// Nobody is.
    SignedOut,
    /// Someone is, and has no organization yet.
    NeedsOrganization {
        /// Who.
        user: UserProfile,
    },
    /// Someone is.
    #[serde(rename_all = "camelCase")]
    SignedIn {
        /// Who.
        user: UserProfile,
        /// Their organization.
        org_id: Option<String>,
    },
}

/// The icon a project action shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectActionIcon {
    /// Run.
    Play,
    /// Test.
    Test,
    /// Lint.
    Lint,
    /// Configure.
    Configure,
    /// Build.
    Build,
    /// Debug.
    Debug,
}

/// A command a project offers in one press.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAction {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// The command it runs.
    pub command: String,
    /// Its icon.
    pub icon: ProjectActionIcon,
    /// Whether it runs on its own when a worktree is made.
    pub run_on_worktree_create: bool,
}

/// A project action before it is saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActionDraft {
    /// Its name.
    pub name: String,
    /// The command it runs.
    pub command: String,
    /// Its icon.
    pub icon: ProjectActionIcon,
    /// Whether it runs on its own when a worktree is made.
    #[serde(default)]
    pub run_on_worktree_create: bool,
}

/// A project's actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActionsSnapshot {
    /// The project.
    pub space_id: String,
    /// Its actions.
    pub actions: Vec<ProjectAction>,
    /// Actions its files offer to import.
    pub importable_actions: Vec<ProjectActionDraft>,
    /// What is wrong with its actions file, when something is.
    pub project_file_issue: Option<String>,
}

/// A project action, started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActionRun {
    /// The action.
    pub action_id: String,
    /// Its name.
    pub action_name: String,
    /// The terminal it runs in.
    pub terminal: TerminalSession,
}

/// An open PTY session on the owning device (`OpenTerminal` reply).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    /// Its id.
    pub id: String,
    /// The folder it started in.
    pub cwd: String,
    /// Shell basename (`zsh`, `bash`, …) for the tab label.
    pub shell: String,
}

/// How this client reaches its machines and its server (the `WatchConnectivity` stream).
///
/// The view model derives it from its links to the workers and to the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connectivity {
    /// How this client reaches its machines.
    pub state: ConnectivityState,
    /// The server this client follows, shown at the sidebar's foot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerPresence>,
}

/// The server this client follows: its name, and whether the link to it is up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPresence {
    /// Its name.
    pub name: String,
    /// Whether the link to it is up.
    pub online: bool,
    /// What the person can do to the server from here (update it), as for a device.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<DeviceAction>,
    /// What a phone or iPad scans to connect to the server, where this device can show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<ServerInvite>,
}

/// The code a phone or iPad scans for the server's address, or why there is none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInvite {
    /// The link the code carries (`slopty://connect?server=<host:port>`); `None` while there is
    /// no address a device reaches the server by.
    pub link: Option<String>,
    /// The address the link names, as the person reads it.
    pub address: Option<String>,
    /// How to use the code, or why there is none.
    pub note: String,
}

/// How this client reaches its machines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectivityState {
    /// No edge transports on this profile (local scope) — hide the pill.
    #[default]
    Disabled,
    /// The OS reports no network path.
    Offline,
    /// Edge expected but the registry room is down (dialing/backing off).
    Reconnecting,
    /// Linked.
    Connected,
}

/// Host capabilities; absent fields keep older peers read-only for mutations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMutationCapabilities {
    /// Whether an entry can be moved or renamed.
    #[serde(default)]
    pub move_entry: bool,
    /// Whether an entry can be deleted.
    #[serde(default)]
    pub delete_entry: bool,
}

/// Rename and move share the same no-replacement operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveWorkspaceEntryRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The change's id.
    pub operation_id: String,
    /// The checkout the entry must still be in.
    pub expected_checkout_id: String,
    /// The entry, workspace-relative.
    pub source_path: String,
    /// Where it goes, workspace-relative; nothing may be there.
    pub destination_path: String,
    /// The revision the entry must still have.
    pub expected_source_revision: String,
    /// What the entry must still be.
    pub expected_kind: WorkspaceEntryKind,
}

/// `DeleteWorkspaceEntry`: a workspace entry deleted, only as it was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteWorkspaceEntryRequest {
    /// The workspace.
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    /// The change's id.
    pub operation_id: String,
    /// The checkout the entry must still be in.
    pub expected_checkout_id: String,
    /// The entry, workspace-relative.
    pub path: String,
    /// The revision the entry must still have.
    pub expected_source_revision: String,
    /// What the entry must still be.
    pub expected_kind: WorkspaceEntryKind,
    /// Explicit consent to delete the directory's current contents.
    pub recursive: bool,
}

/// What came of a move or a delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WorkspaceMutationOutcome {
    /// It was made.
    #[serde(rename_all = "camelCase")]
    Applied {
        /// The change's id.
        operation_id: String,
        /// The checkout it was made in.
        checkout_id: String,
        /// What changed.
        change: WorkspaceFileChange,
        /// The entry as it is now, when it still is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<WorkspaceEntry>,
    },
    /// The change was refused, and nothing changed.
    #[serde(rename_all = "camelCase")]
    Rejected {
        /// The change's id.
        operation_id: String,
        /// Why.
        reason: WorkspaceMutationRejection,
        /// Why, in words.
        message: String,
    },
}

/// Why a move or a delete was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceMutationRejection {
    /// A path is not one in the workspace.
    InvalidPath,
    /// The workspace is not the checkout it was.
    WorkspaceChanged,
    /// The entry is gone.
    SourceMissing,
    /// The entry changed since it was read.
    SourceChanged,
    /// Something is already where it would go.
    DestinationExists,
    /// It cannot go there.
    InvalidDestination,
    /// The machine does not allow it.
    PermissionDenied,
    /// The machine does not offer it.
    Unsupported,
    /// Another change is under way.
    Busy,
    /// It was made only in part.
    PartialFailure,
}

#[cfg(test)]
mod mutation_contract_tests {
    use super::*;
    #[test]
    fn old_peers_remain_readable_without_mutation_support() {
        let page: WorkspaceDirectoryPage = serde_json::from_value(serde_json::json!({
            "directory":"", "entries":[{"path":"a", "name":"a", "kind":"file", "ignored":false, "readOnly":false}],
            "truncated":false
        })).unwrap();
        assert_eq!(page.checkout_id, None);
        assert_eq!(page.mutation_capabilities, None);
        assert_eq!(page.entries[0].mutation_revision, None);
        let change: WorkspaceFileChange =
            serde_json::from_value(serde_json::json!({"kind":"removed", "path":"a"})).unwrap();
        assert_eq!(change.operation_id, None);
    }
    #[test]
    fn mutation_requests_and_outcomes_round_trip() {
        let request = serde_json::json!({"chatId":"chat", "operationId":"op", "expectedCheckoutId":"checkout", "sourcePath":"a", "destinationPath":"b", "expectedSourceRevision":"rev", "expectedKind":"file"});
        let decoded: MoveWorkspaceEntryRequest = serde_json::from_value(request.clone()).unwrap();
        let encoded = serde_json::to_value(decoded).unwrap();
        for (key, value) in request.as_object().unwrap() {
            assert_eq!(&encoded[key], value);
        }
        let delete: DeleteWorkspaceEntryRequest = serde_json::from_value(serde_json::json!({"chatId":"chat", "operationId":"op", "expectedCheckoutId":"checkout", "path":"a", "expectedSourceRevision":"rev", "expectedKind":"directory", "recursive":true})).unwrap();
        assert_eq!(
            serde_json::from_value::<DeleteWorkspaceEntryRequest>(
                serde_json::to_value(&delete).unwrap()
            )
            .unwrap(),
            delete
        );
        let outcome = WorkspaceMutationOutcome::Rejected {
            operation_id: "op".into(),
            reason: WorkspaceMutationRejection::DestinationExists,
            message: "Destination exists".into(),
        };
        assert_eq!(
            serde_json::from_value::<WorkspaceMutationOutcome>(
                serde_json::to_value(&outcome).unwrap()
            )
            .unwrap(),
            outcome
        );
        serde_json::from_value::<WorkspaceMutationRejection>(serde_json::json!("overwrite"))
            .unwrap_err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_workspace_reads_round_trip_without_breaking_older_reasons() {
        let reason = WorkspaceReadOnlyReason::OutsideWorkspace;
        assert_eq!(serde_json::to_value(reason).unwrap(), "outsideWorkspace");
        assert_eq!(
            serde_json::from_value::<WorkspaceReadOnlyReason>(serde_json::json!("symlink"))
                .unwrap(),
            WorkspaceReadOnlyReason::Symlink,
            "older peers keep parsing every reason they knew"
        );
    }

    #[test]
    fn checkout_file_diff_text_contract_is_camel_case() {
        let request = GetCheckoutFileDiffTextRequest {
            checkout_id: "checkout".into(),
            cwd: "/repo".into(),
            path: "src/lib.rs".into(),
            mode: "branch".into(),
            base_ref: Some("main".into()),
            chat_id: None,
            commit_sha: Some("deadbeef".into()),
            diff_checksum: "abc".into(),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["checkoutId"], "checkout");
        assert_eq!(value["diffChecksum"], "abc");
        assert_eq!(value["commitSha"], "deadbeef");
        assert_eq!(
            serde_json::from_value::<GetCheckoutFileDiffTextRequest>(value).unwrap(),
            request
        );
    }

    #[test]
    fn workspace_file_requests_flatten_target_and_omit_options() {
        let request = ListWorkspaceDirectoryRequest {
            target: WorkspaceTarget {
                chat_id: Some("chat-1".into()),
                space_id: None,
                checkout_path: None,
            },
            directory: "src/日本語".into(),
            include_ignored: false,
            cursor: None,
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "chatId": "chat-1",
                "directory": "src/日本語",
                "includeIgnored": false,
            })
        );
        assert_eq!(
            serde_json::from_value::<ListWorkspaceDirectoryRequest>(value).unwrap(),
            request
        );

        let requests = [
            serde_json::to_value(SearchWorkspaceFilesRequest {
                target: request.target.clone(),
                query: "main".into(),
                include_ignored: false,
                limit: None,
            })
            .unwrap(),
            serde_json::to_value(ReadWorkspaceFileRequest {
                target: request.target.clone(),
                path: "src/main.rs".into(),
            })
            .unwrap(),
            serde_json::to_value(WatchWorkspaceFilesRequest { target: request.target }).unwrap(),
        ];
        assert!(requests.iter().all(|value| value["chatId"] == "chat-1"));
    }

    #[test]
    fn warm_workspace_search_flattens_its_target() {
        // Early clients also sent a `pin` flag; it is ignored now.
        let request: WarmWorkspaceSearchRequest =
            serde_json::from_value(serde_json::json!({ "chatId": "chat-1", "pin": true })).unwrap();
        assert_eq!(request.target.chat_id.as_deref(), Some("chat-1"));
        assert_eq!(
            serde_json::to_value(WarmWorkspaceSearchResult {
                state: WorkspaceSearchIndexState::Building,
            })
            .unwrap(),
            serde_json::json!({ "state": "building" })
        );
    }

    #[test]
    fn workspace_write_outcomes_have_stable_tags() {
        let written = WriteWorkspaceFileOutcome::Written {
            file: WorkspaceFileWriteResult {
                path: "src/emoji-🛰️.rs".into(),
                content_hash: "hash-2".into(),
                size: 12,
                modified_at: None,
            },
        };
        assert_eq!(
            serde_json::to_value(&written).unwrap(),
            serde_json::json!({
                "status": "written",
                "file": {
                    "path": "src/emoji-🛰️.rs",
                    "contentHash": "hash-2",
                    "size": 12,
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<WriteWorkspaceFileOutcome>(
                serde_json::to_value(&written).unwrap()
            )
            .unwrap(),
            written
        );

        let conflict = WriteWorkspaceFileOutcome::Conflict {
            reason: WorkspaceFileConflictReason::Changed,
            current_content_hash: Some("hash-3".into()),
            current_modified_at: None,
        };
        assert_eq!(
            serde_json::to_value(&conflict).unwrap(),
            serde_json::json!({
                "status": "conflict",
                "reason": "changed",
                "currentContentHash": "hash-3",
            })
        );
    }

    #[test]
    fn workspace_file_change_contract_is_camel_case() {
        let changes = WorkspaceFileChanges {
            sequence: 4,
            resync_required: false,
            changes: vec![WorkspaceFileChange {
                operation_id: None,
                kind: WorkspaceFileChangeKind::Renamed,
                path: "src/new.rs".into(),
                old_path: Some("src/old.rs".into()),
            }],
        };
        let value = serde_json::to_value(&changes).unwrap();
        assert_eq!(value["resyncRequired"], false);
        assert_eq!(value["changes"][0]["kind"], "renamed");
        assert_eq!(value["changes"][0]["oldPath"], "src/old.rs");
        assert_eq!(serde_json::from_value::<WorkspaceFileChanges>(value).unwrap(), changes);
    }
}
