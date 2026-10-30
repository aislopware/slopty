// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! The entities the view model serves the shell: devices, projects (spaces), chats and their
//! sessions, the links to the server, and the requests and answers of a machine's files, git,
//! pull requests, project actions and terminals.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{HarnessId, ReasoningLevel, SandboxLevel};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    /// First registration time, for the Devices page's "Added …" fragment.
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// App version the device's engine last booted with — fleet staleness at a
    /// glance (Devices page). Optional so pre-existing docs stay readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Cursor SDK selected by the owning engine; absent on older engines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_sdk_version: Option<String>,
    /// Protocol/document features supported by the engine currently owning
    /// this device row. Missing on older builds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    // What the person can do to the machine from here (update, remove, wake…), as the
    // app offers it; the Devices page shows each and calls `DeviceAction` with its id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<DeviceAction>,
    // Where this client reaches the machine (its tailnet name or address), for the
    // person's own tools: an editor's SSH remote. None for this machine's own worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    // Why the machine turns this device away, in words for its badge's tooltip (its
    // tailnet policy grants this device no role there, or its allow ranges do not hold this
    // device's address); None while it lets this device in. The badge retries with the row's
    // `DeviceAction::REDIAL` action and never asks for a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turned_away: Option<String>,
}

// One thing the person can do to a machine or the server, named by the app.
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

impl Device {
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }
}

/// A synced (device, folder) pair — the unit of organization in the sidebar.
/// Sessions belong to exactly one space; the space fixes their host device and
/// base cwd. Folders need not be git repos: `git_detected` is stamped by the
/// owning device (SpacesSync) and gates branch pickers / the diff sidebar on
/// every device without an RPC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Space {
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
    /// (sha256(deviceId ‖ NUL ‖ git_dir)) — diff grouping key for root sessions.
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
        trimmed
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConfig {
    pub harness: HarnessId,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub sandbox: SandboxLevel,
}

/// Immutable-at-run-start repository context owned by one conversation.
///
/// This is deliberately separate from the live checkout snapshot: another
/// chat may change the branch at the same checkout without changing which
/// branch this conversation belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSourceContext {
    pub checkout_id: String,
    pub repo_root: String,
    pub cwd: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    /// Owning (host) device.
    pub device_id: String,
    pub title: Option<String>,
    pub archived: bool,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    /// Canonical id of the repo checkout/worktree this chat operates in.
    pub checkout_id: Option<String>,
    /// Repository identity captured for this conversation immediately before
    /// its harness run. Unlike `branch`, this is never inferred from another
    /// chat sharing the same checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_context: Option<ConversationSourceContext>,
    pub config: Option<ChatConfig>,
    pub last_message_preview: Option<String>,
    pub last_message_at: Option<DateTime<Utc>>,
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
    /// Which sync room generation serves this chat (docs/chat2-sync.md M2):
    /// `None`/1 = legacy s2 loro room, 2 = chat2 dumb relay. The HOST flips
    /// this in the same breath as seeding the chat2 checkpoint; every device
    /// dials the room the registry names. Per-chat and instantly revertible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_gen: Option<u32>,
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
    // What the person was writing to it and has not sent, as a device last kept it on
    // its machine; the composer restores it on open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
}

impl Chat {
    /// A session the user sees in chat lists: not another chat's worker
    /// (`parent_chat_id`).
    pub fn is_top_level(&self) -> bool {
        self.parent_chat_id.is_none()
    }

    /// True when this chat syncs over the chat2 dumb relay.
    pub fn on_chat2(&self) -> bool {
        self.room_gen.unwrap_or(1) >= 2
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
    Working,
    AwaitingInput,
    Errored,
    /// Finished running (or errored out) but not seen yet on any device.
    Completed,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    Idle,
    Working,
    AwaitingInput,
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
    /// The agent asks the person questions.
    pub const QUESTION: &'static str = "question";
    /// A plan waits for approval.
    pub const PLAN: &'static str = "plan";
    /// A call waits to be allowed.
    pub const APPROVAL: &'static str = "approval";
    /// The agent's context is being summarised.
    pub const COMPACTING: &'static str = "compacting";
}

/// Live run status for a chat — drives the Working indicator and sidebar status dots.
/// Staleness-checked client-side against `updated_at` so a crashed backend never shows
/// an eternal "Working".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Last successfully completed assistant turn. Retained while the next turn
    /// runs so coalesced status watches do not lose normal queue completions.
    /// Interrupts, failures and liveness expiry never advance this marker.
    #[serde(default)]
    pub last_completed_turn: Option<String>,
    pub chat_id: String,
    pub device_id: String,
    pub status: SessionStatus,
    pub started_at: Option<DateTime<Utc>>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repo {
    pub path: String,
    pub name: String,
    pub default_branch: Option<String>,
}

/// One row of `ListRefs`: a branch plus its checkout state — whether it is
/// the repo's current (main-checkout) branch and whether it is materialized
/// as a linked worktree. Drives the composer's ref picker (`current` /
/// `worktree` tags) and the checkout-kind selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRef {
    pub name: String,
    /// Checked out in the repo's MAIN folder right now.
    #[serde(default)]
    pub current: bool,
    /// Path of the linked worktree this branch is checked out in, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// `origin`'s default branch (`origin/HEAD`), which the picker tags "default".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub repo_path: String,
    pub path: String,
    pub branch: String,
    /// Generated worktree folder name (`slopty/<name>` is its branch).
    #[serde(default)]
    pub name: String,
    /// Canonical checkout identity (device-scoped hash of the git dir).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderListing {
    pub path: String,
    pub entries: Vec<FolderEntry>,
    /// True when the listing hit the entry cap.
    #[serde(default)]
    pub truncated: bool,
}

/// Device-local preferences, used only when creating a new worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorktreeSettings {
    pub use_custom_directory: bool,
    /// Remembered even while the custom location is disabled.
    pub custom_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSettingsStatus {
    pub settings: WorktreeSettings,
    pub default_directory: String,
    pub effective_directory: String,
    /// An explicit host environment override takes precedence over preferences.
    pub environment_override: Option<String>,
}

/// `SearchHomeFolders`: folders anywhere under the host's home whose names
/// match `query`, for the new-project picker. An empty query only starts the
/// host's home search index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHomeFoldersRequest {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeFolderMatch {
    /// Absolute path on the host.
    pub path: String,
    /// Home-relative, `/`-separated (what the picker shows).
    pub relative: String,
    pub is_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeFolderSearchResult {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveListing {
    pub drives: Vec<DriveEntry>,
}

/// A workspace-relative file or directory returned by `SearchFiles`.
/// Contents deliberately never cross this boundary: mentioning a path leaves
/// the harness to read it through its normal workspace tools when needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchMatch {
    pub path: String,
    pub is_dir: bool,
}

/// Identifies the local checkout used by workspace file operations.
/// Exactly one of `chat_id` and `space_id` must be present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListWorkspaceDirectoryRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub include_ignored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDirectoryPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_capabilities: Option<WorkspaceMutationCapabilities>,
    pub directory: String,
    pub entries: Vec<WorkspaceEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_revision: Option<String>,
    pub path: String,
    pub name: String,
    pub kind: WorkspaceEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
    pub ignored: bool,
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceEntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWorkspaceFilesRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub query: String,
    #[serde(default)]
    pub include_ignored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileSearchMatch {
    pub path: String,
    pub name: String,
    pub kind: WorkspaceEntryKind,
    pub score: i64,
}

/// Plain-text search over a workspace's file contents (smart case). Binary
/// and oversized files are skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWorkspaceContentRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_file_limit: Option<u16>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceContentSearchResult {
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
    #[serde(flatten)]
    pub target: WorkspaceTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceSearchIndexState {
    /// The initial scan is still running; searches may be incomplete.
    Building,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmWorkspaceSearchResult {
    pub state: WorkspaceSearchIndexState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWorkspaceFileRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub path: String,
}

/// Workspace images travel in bounded relay frames, independently of text reads.
pub const MAX_WORKSPACE_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const WORKSPACE_IMAGE_CHUNK_BYTES: usize = 384 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWorkspaceImageRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub path: String,
    pub expected_checkout_id: String,
    pub offset: usize,
    pub expected_content_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceImageChunk {
    pub checkout_id: String,
    pub content_hash: String,
    pub mime_type: String,
    pub data: String,
    pub next_offset: usize,
    pub size: usize,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileText {
    /// Identity of the checkout this snapshot was read from.
    pub checkout_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
    pub encoding: WorkspaceTextEncoding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_ending: Option<WorkspaceLineEnding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_reason: Option<WorkspaceReadOnlyReason>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceTextEncoding {
    Utf8,
    Utf8Bom,
    Binary,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceLineEnding {
    Lf,
    Crlf,
    Mixed,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceReadOnlyReason {
    Binary,
    UnsupportedEncoding,
    MixedLineEndings,
    Symlink,
    TooLarge,
    PermissionDenied,
    NotRegularFile,
    /// A file read by absolute path beyond the chat's workspace root: reads
    /// are allowed, writes never are. Older peers never see this variant —
    /// only a new UI asks for an absolute path — so the wire stays additive.
    OutsideWorkspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteWorkspaceFileRequest {
    /// Must match the read snapshot, even if the chat has since changed cwd.
    pub expected_checkout_id: String,
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub path: String,
    pub text: String,
    pub expected_content_hash: String,
    pub encoding: WorkspaceWritableEncoding,
    pub line_ending: WorkspaceWritableLineEnding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceWritableEncoding {
    Utf8,
    Utf8Bom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceWritableLineEnding {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WriteWorkspaceFileOutcome {
    #[serde(rename_all = "camelCase")]
    Written { file: WorkspaceFileWriteResult },
    #[serde(rename_all = "camelCase")]
    Conflict {
        reason: WorkspaceFileConflictReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_content_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_modified_at: Option<DateTime<Utc>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileWriteResult {
    pub path: String,
    pub content_hash: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceFileConflictReason {
    Changed,
    Deleted,
    Replaced,
    NotRegularFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchWorkspaceFilesRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileChanges {
    pub sequence: u64,
    pub resync_required: bool,
    pub changes: Vec<WorkspaceFileChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub kind: WorkspaceFileChangeKind,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceFileChangeKind {
    Created,
    Modified,
    Removed,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFileSummary {
    pub path: String,
    /// Previous path for renames/copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    #[serde(default)]
    pub binary: bool,
}

/// Git porcelain states, independent of patch size and line counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitFileState {
    Unchanged,
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
    TypeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileStatus {
    pub path: String,
    pub old_path: Option<String>,
    pub index: GitFileState,
    pub worktree: GitFileState,
}

/// Latest status only: never contains file content or a patch. `complete = false`
/// means unavailable/partial, not clean. Revision covers only these statuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutGitStatus {
    pub checkout_id: String,
    pub device_id: String,
    pub revision: String,
    pub complete: bool,
    pub files: Vec<GitFileStatus>,
}

/// Keep unavailable updates inside an object: the RPC envelope uses JSON null
/// for a missing item, so a bare optional snapshot cannot signal invalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGitStatusFrame {
    pub status: Option<CheckoutGitStatus>,
}

/// Working-tree diff for a checkout — latest-only sidecar, 3MiB patch cap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutDiff {
    pub checkout_id: String,
    pub device_id: String,
    pub cwd: String,
    pub patch: String,
    pub files: Vec<DiffFileSummary>,
    pub additions: u32,
    pub deletions: u32,
    /// True when the patch was truncated at the byte cap ("Partial snapshot").
    pub truncated: bool,
    pub checksum: String,
    pub updated_at: DateTime<Utc>,
}

/// Provider-neutral lifecycle state for a code change request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeRequestState {
    Open,
    Closed,
    Merged,
}

/// Whether a change request can be merged without resolving conflicts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeRequestMergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

/// Aggregate review decision reported for a change request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeRequestReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    #[default]
    Unknown,
}

/// Compact provider-neutral change request metadata for checkout surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequestSummary {
    pub provider: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: ChangeRequestState,
    pub base_ref: String,
    pub head_ref: String,
}

/// Relationship filter for one repository; never widens repository scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeRequestFilter {
    #[default]
    Authored,
    All,
    Reviewing,
}

/// Provider-neutral change request metadata for global listing surfaces.
///
/// Unlike [`ChangeRequestSummary`], this contract is not tied to a checkout
/// and therefore identifies the repository instead of requiring branch refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequestListItem {
    pub provider: String,
    #[serde(default)]
    pub author: ChangeRequestActor,
    pub repository: String,
    /// Current source commit, captured with the CI rollup in one provider request.
    #[serde(default)]
    pub head_ref_oid: String,
    #[serde(default)]
    pub ci: crate::change_request_assessment::ChangeRequestCi,
    /// None means an older host or unavailable viewer relationship metadata.
    #[serde(default)]
    pub viewer_did_author: Option<bool>,
    #[serde(default)]
    pub viewer_review_requested: Option<bool>,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: ChangeRequestState,
    pub is_draft: bool,
    /// `Unknown` when the provider reports no decision.
    #[serde(default)]
    pub review_decision: ChangeRequestReviewDecision,
    pub additions: u64,
    pub deletions: u64,
    pub mergeability: ChangeRequestMergeability,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One page of a repository-scoped listing, newest updates first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestPage {
    pub items: Vec<ChangeRequestListItem>,
    /// Opaque provider cursor for the following page; `None` on the last one.
    pub next_cursor: Option<String>,
    /// Every match for the query, when the provider reports it.
    pub total_count: Option<u64>,
    /// When the engine fetched this page from the provider. A page served
    /// from the engine's on-disk copy carries its original time, so clients
    /// can show it at once and refresh it when it has aged. Absent from
    /// engines that predate the copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<DateTime<Utc>>,
}

/// Read-only GitHub detail response. Optional collections tolerate absent provider data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestDetail {
    pub title: String,
    pub body: String,
    pub url: String,
    pub number: u64,
    pub author: ChangeRequestActor,
    pub base_ref_name: String,
    pub base_ref_oid: String,
    pub head_ref_name: String,
    pub head_ref_oid: String,
    pub ci: crate::change_request_assessment::ChangeRequestCi,
    pub viewer_did_author: Option<bool>,
    pub viewer_review_requested: Option<bool>,
    pub state: String,
    pub is_draft: bool,
    pub review_decision: String,
    pub mergeable: String,
    pub additions: u64,
    pub deletions: u64,
    pub comments: Vec<ChangeRequestComment>,
    pub reviews: Vec<ChangeRequestComment>,
    /// None means unavailable (including an older engine), not no feedback.
    pub review_threads: Option<Vec<ChangeRequestReviewThread>>,
    pub files: Vec<ChangeRequestFile>,
    pub status_check_rollup: Vec<ChangeRequestCheck>,
    /// The signed-in GitHub account, so clients can tell which comments and
    /// reviews are the viewer's. Absent from older engines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer_login: Option<String>,
}

impl ChangeRequestDetail {
    pub fn activity_comments(&self) -> impl Iterator<Item = &ChangeRequestComment> {
        self.comments.iter().chain(&self.reviews).chain(
            self.review_threads
                .iter()
                .flatten()
                .flat_map(|thread| &thread.comments),
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestReviewThread {
    pub id: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub path: String,
    pub line: Option<u64>,
    pub start_line: Option<u64>,
    pub original_line: Option<u64>,
    pub original_start_line: Option<u64>,
    pub diff_side: String,
    pub start_diff_side: Option<String>,
    pub comments: Vec<ChangeRequestComment>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestActor {
    pub login: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestComment {
    pub id: String,
    pub url: String,
    pub reply_to: Option<ChangeRequestCommentReference>,
    /// Provided by GitHub for the authenticated viewer, independent of PR authorship.
    pub viewer_did_author: bool,
    pub author: ChangeRequestActor,
    pub body: String,
    pub state: String,
    pub created_at: String,
    pub submitted_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestCommentReference {
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangeRequestCheck {
    pub name: String,
    pub context: String,
    pub status: String,
    pub conclusion: String,
    pub state: String,
    pub details_url: String,
    pub target_url: String,
}

/// Latest successful change request resolution for one checkout and branch.
///
/// `change_request: None` is an authoritative successful lookup with no match;
/// resolution failures must retain the previous successful snapshot instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutChangeRequestStatus {
    pub checkout_id: String,
    pub device_id: String,
    pub cwd: String,
    pub branch: String,
    pub change_request: Option<ChangeRequestSummary>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCheckoutFileDiffTextRequest {
    pub checkout_id: String,
    pub cwd: String,
    pub path: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// Pinned commit for History's per-commit diff scope. When present, the
    /// source pair is read from the commit parent and this commit, never from
    /// the live working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    pub diff_checksum: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutFileDiffText {
    pub diff_checksum: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_content_hash: Option<String>,
    pub binary: bool,
    pub truncated: bool,
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum AuthState {
    SignedOut,
    NeedsOrganization {
        user: UserProfile,
    },
    #[serde(rename_all = "camelCase")]
    SignedIn {
        user: UserProfile,
        org_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectActionIcon {
    Play,
    Test,
    Lint,
    Configure,
    Build,
    Debug,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAction {
    pub id: String,
    pub name: String,
    pub command: String,
    pub icon: ProjectActionIcon,
    pub run_on_worktree_create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActionDraft {
    pub name: String,
    pub command: String,
    pub icon: ProjectActionIcon,
    #[serde(default)]
    pub run_on_worktree_create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActionsSnapshot {
    pub space_id: String,
    pub actions: Vec<ProjectAction>,
    pub importable_actions: Vec<ProjectActionDraft>,
    pub project_file_issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActionRun {
    pub action_id: String,
    pub action_name: String,
    pub terminal: TerminalSession,
}

/// Result of creating a worktree. The worktree remains flattened so this is
/// wire-compatible with both legacy callers and legacy engine replies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorktreeOutcome {
    #[serde(flatten)]
    pub worktree: Worktree,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_action: Option<ProjectActionRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_error: Option<String>,
}

/// An open PTY session on the owning device (`OpenTerminal` reply).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub id: String,
    pub cwd: String,
    /// Shell basename (`zsh`, `bash`, …) for the tab label.
    pub shell: String,
}

/// One `SubscribeTerminal` stream item. `seq` is a per-terminal monotonic counter
/// used for replay resumption (`afterSeq`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    /// Output chunk; `data` is base64 (PTY output is raw bytes, not valid UTF-8).
    Data { seq: u64, data: String },
    #[serde(rename_all = "camelCase")]
    Exit {
        seq: u64,
        exit_code: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },
}

/// Live edge-connectivity posture (the `WatchConnectivity` stream): the truth
/// the connection pill, composer honesty, and queued-send badges render.
/// Derived engine-side from the registry room's reconnect state, the OS
/// network-path monitor, and each open chat room's stats.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connectivity {
    pub state: ConnectivityState,
    /// Epoch ms of the next scheduled registry dial while reconnecting
    /// (0 = none pending / dialing right now). The countdown renders
    /// client-side from this.
    #[serde(default)]
    pub retry_at_ms: i64,
    /// The failure that started the current outage — sticky through the next
    /// attempt (no flicker back to a bare "connecting…"), cleared on rejoin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    /// Per-OPEN-chat room state; a chat absent here is unknown (consumers
    /// fall back to the global state).
    #[serde(default)]
    pub chats: Vec<ChatConnectivity>,
    // The server this client follows, shown at the sidebar's foot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerPresence>,
}

// The server this client follows: its name, and whether the link to it is up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPresence {
    pub name: String,
    pub online: bool,
    /// What the person can do to the server from here (update it), as for a device.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<DeviceAction>,
    // What a phone or iPad scans to connect to the server, where this device can show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<ServerInvite>,
}

// The code a phone or iPad scans for the server's address, or why there is none.
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
    Connected,
}

/// Additive per-chat admission status; legacy peers omit it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatSyncState {
    Local,
    Waiting,
    Connecting,
    Synced,
    Offline,
    StorageError,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConnectivity {
    pub chat_id: String,
    #[serde(default)]
    pub sync_state: ChatSyncState,
    /// Grace-filtered per-chat health; true does not prove live delivery.
    pub connected: bool,
    /// This chat can currently deliver over its room or HTTP fallback.
    /// Older engines omit it, so consumers conservatively assume false.
    #[serde(default)]
    pub delivery_live: bool,
    /// Local update batches not yet acked by the chat's edge room.
    #[serde(default)]
    pub pending_pushes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
    fn legacy_chat_connectivity_has_no_live_delivery_proof() {
        let chat: ChatConnectivity = serde_json::from_value(serde_json::json!({
            "chatId": "remote",
            "connected": true,
            "syncState": "synced"
        }))
        .unwrap();
        assert!(!chat.delivery_live);
        let mut live = chat;
        live.delivery_live = true;
        assert_eq!(serde_json::to_value(live).unwrap()["deliveryLive"], true);
    }

    #[test]
    fn checkout_change_request_status_round_trips_all_states_as_camel_case() {
        for (state, encoded_state) in [
            (ChangeRequestState::Open, "open"),
            (ChangeRequestState::Closed, "closed"),
            (ChangeRequestState::Merged, "merged"),
        ] {
            let status = CheckoutChangeRequestStatus {
                checkout_id: "checkout-1".into(),
                device_id: "device-1".into(),
                cwd: "/repo".into(),
                branch: "feature/change".into(),
                change_request: Some(ChangeRequestSummary {
                    provider: "github".into(),
                    number: 90,
                    title: "Model checkout change request status".into(),
                    url: "https://github.com/acme/slopty/pull/90".into(),
                    state,
                    base_ref: "main".into(),
                    head_ref: "feature/change".into(),
                }),
                updated_at: Utc.with_ymd_and_hms(2026, 8, 15, 12, 30, 0).unwrap(),
            };

            let value = serde_json::to_value(&status).unwrap();
            assert_eq!(value["checkoutId"], "checkout-1");
            assert_eq!(value["deviceId"], "device-1");
            assert_eq!(value["changeRequest"]["state"], encoded_state);
            assert_eq!(value["changeRequest"]["baseRef"], "main");
            assert_eq!(value["changeRequest"]["headRef"], "feature/change");
            assert_eq!(
                serde_json::from_value::<CheckoutChangeRequestStatus>(value).unwrap(),
                status
            );
        }
    }

    #[test]
    fn change_request_list_item_round_trips_all_states_as_camel_case() {
        for (state, encoded_state) in [
            (ChangeRequestState::Open, "open"),
            (ChangeRequestState::Closed, "closed"),
            (ChangeRequestState::Merged, "merged"),
        ] {
            let item = ChangeRequestListItem {
                provider: "github".into(),
                author: Default::default(),
                head_ref_oid: String::new(),
                ci: Default::default(),
                viewer_did_author: None,
                viewer_review_requested: None,
                repository: "private-owner/private-repo".into(),
                number: 123,
                title: "Add pull request dashboard".into(),
                url: "https://github.com/private-owner/private-repo/pull/123".into(),
                state,
                is_draft: true,
                review_decision: ChangeRequestReviewDecision::ChangesRequested,
                additions: 42,
                deletions: 7,
                mergeability: ChangeRequestMergeability::Conflicting,
                created_at: Utc.with_ymd_and_hms(2026, 8, 10, 9, 30, 0).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap(),
            };

            let value = serde_json::to_value(&item).unwrap();
            assert_eq!(value["repository"], "private-owner/private-repo");
            assert_eq!(value["state"], encoded_state);
            assert_eq!(value["isDraft"], true);
            assert_eq!(value["reviewDecision"], "changesRequested");
            assert_eq!(value["additions"], 42);
            assert_eq!(value["deletions"], 7);
            assert_eq!(value["mergeability"], "conflicting");
            assert_eq!(value["createdAt"], "2026-08-10T09:30:00Z");
            assert_eq!(value["updatedAt"], "2026-08-19T12:00:00Z");
            assert!(value.get("baseRef").is_none());
            assert!(value.get("headRef").is_none());
            assert_eq!(
                serde_json::from_value::<ChangeRequestListItem>(value).unwrap(),
                item
            );
        }
    }

    #[test]
    fn change_request_mergeability_round_trips_all_states() {
        for (state, encoded) in [
            (ChangeRequestMergeability::Mergeable, "mergeable"),
            (ChangeRequestMergeability::Conflicting, "conflicting"),
            (ChangeRequestMergeability::Unknown, "unknown"),
        ] {
            let value = serde_json::to_value(state).unwrap();
            assert_eq!(value, encoded);
            assert_eq!(
                serde_json::from_value::<ChangeRequestMergeability>(value).unwrap(),
                state
            );
        }
    }

    #[test]
    fn change_request_review_decision_round_trips_and_defaults_when_absent() {
        for (decision, encoded) in [
            (ChangeRequestReviewDecision::Approved, "approved"),
            (
                ChangeRequestReviewDecision::ChangesRequested,
                "changesRequested",
            ),
            (
                ChangeRequestReviewDecision::ReviewRequired,
                "reviewRequired",
            ),
            (ChangeRequestReviewDecision::Unknown, "unknown"),
        ] {
            let value = serde_json::to_value(decision).unwrap();
            assert_eq!(value, encoded);
            assert_eq!(
                serde_json::from_value::<ChangeRequestReviewDecision>(value).unwrap(),
                decision
            );
        }

        let mut value = serde_json::to_value(ChangeRequestListItem {
            provider: "github".into(),
            author: Default::default(),
            head_ref_oid: String::new(),
            ci: Default::default(),
            viewer_did_author: None,
            viewer_review_requested: None,
            repository: "acme/slopty".into(),
            number: 1,
            title: "No decision".into(),
            url: "https://github.com/acme/slopty/pull/1".into(),
            state: ChangeRequestState::Open,
            is_draft: false,
            review_decision: ChangeRequestReviewDecision::Approved,
            additions: 1,
            deletions: 0,
            mergeability: ChangeRequestMergeability::Mergeable,
            created_at: Utc.with_ymd_and_hms(2026, 8, 10, 9, 30, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap(),
        })
        .unwrap();
        value.as_object_mut().unwrap().remove("reviewDecision");
        assert_eq!(
            serde_json::from_value::<ChangeRequestListItem>(value)
                .unwrap()
                .review_decision,
            ChangeRequestReviewDecision::Unknown
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
    fn create_worktree_outcome_accepts_legacy_reply_and_stays_flattened() {
        let legacy = serde_json::json!({
            "repoPath": "/repo",
            "path": "/worktree",
            "branch": "slopty/branch",
            "name": "branch",
            "checkoutId": "checkout",
        });
        let outcome: CreateWorktreeOutcome = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(outcome.worktree.path, "/worktree");
        assert!(outcome.setup_action.is_none());
        assert!(outcome.setup_error.is_none());

        let encoded = serde_json::to_value(outcome).unwrap();
        assert_eq!(encoded["path"], legacy["path"]);
        assert!(encoded.get("worktree").is_none());
        assert!(encoded.get("setupAction").is_none());
        assert!(encoded.get("setupError").is_none());
        assert!(serde_json::from_value::<Worktree>(encoded).is_ok());
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
            serde_json::to_value(WatchWorkspaceFilesRequest {
                target: request.target,
            })
            .unwrap(),
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
        assert_eq!(
            serde_json::from_value::<WorkspaceFileChanges>(value).unwrap(),
            changes
        );
    }
}

/// Host capabilities; absent fields keep older peers read-only for mutations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMutationCapabilities {
    #[serde(default)]
    pub move_entry: bool,
    #[serde(default)]
    pub delete_entry: bool,
}

/// Rename and move share the same no-replacement operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveWorkspaceEntryRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub operation_id: String,
    pub expected_checkout_id: String,
    pub source_path: String,
    pub destination_path: String,
    pub expected_source_revision: String,
    pub expected_kind: WorkspaceEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteWorkspaceEntryRequest {
    #[serde(flatten)]
    pub target: WorkspaceTarget,
    pub operation_id: String,
    pub expected_checkout_id: String,
    pub path: String,
    pub expected_source_revision: String,
    pub expected_kind: WorkspaceEntryKind,
    /// Explicit consent to delete the directory's current contents.
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WorkspaceMutationOutcome {
    #[serde(rename_all = "camelCase")]
    Applied {
        operation_id: String,
        checkout_id: String,
        change: WorkspaceFileChange,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<WorkspaceEntry>,
    },
    #[serde(rename_all = "camelCase")]
    Rejected {
        operation_id: String,
        reason: WorkspaceMutationRejection,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceMutationRejection {
    InvalidPath,
    WorkspaceChanged,
    SourceMissing,
    SourceChanged,
    DestinationExists,
    InvalidDestination,
    PermissionDenied,
    Unsupported,
    Busy,
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
        assert!(
            serde_json::from_value::<WorkspaceMutationRejection>(serde_json::json!("overwrite"))
                .is_err()
        );
    }
}
