// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Frontend-agnostic view logic: the derivations every viewport needs and none
//! of them should own — sort orders, staleness gating, sidebar grouping, the
//! boot gate, relative times.
//!
//! This lives in `proto` rather than in the viewport crate so the rules stay
//! pure and independently testable: the same workspace doc must produce the
//! same row order on every surface, and there is exactly one implementation
//! and one test suite per rule.
//!
//! Everything in this module is pure. `chat_indicator` (the status derivation
//! these gate on) is in [`crate::entities`].

use chrono::{DateTime, Utc};

use crate::{AuthState, Chat, ChatIndicator, Session, SessionStatus, Space};

// ---------------------------------------------------------------------------
// Connection + status
// ---------------------------------------------------------------------------

/// Viewport ⇄ engine connection lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Not reached yet.
    Connecting,
    /// Reached and answering.
    Ready,
    /// Could not be reached, and why.
    Failed(String),
}

/// What a chat's status dot / working indicator should show right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indicator {
    /// Nothing live: at rest, done, stopped, or a session gone stale.
    None,
    /// The agent works.
    Working,
    /// The agent waits on the person.
    AwaitingInput,
    /// The agent failed.
    Errored,
}

/// A `Working`/`AwaitingInput` session older than this is treated as dead — a
/// crashed backend must never show an eternal "Working". Engines heartbeat sessions well
/// inside this window.
pub const SESSION_STALE_MS: i64 = 45_000;

/// Staleness-checked indicator for a session row. Pure.
pub fn effective_indicator(session: Option<&Session>, now: DateTime<Utc>) -> Indicator {
    let Some(session) = session else {
        return Indicator::None;
    };
    match session.status {
        // An agent at rest, waiting on its own work, or done or stopped shows no live
        // state; the sidebar reads which from the session itself.
        SessionStatus::Idle
        | SessionStatus::Waiting
        | SessionStatus::Done
        | SessionStatus::Stopped => Indicator::None,
        SessionStatus::Errored => Indicator::Errored,
        SessionStatus::Working | SessionStatus::AwaitingInput => {
            let age_ms = now.signed_duration_since(session.updated_at).num_milliseconds();
            if age_ms > SESSION_STALE_MS {
                Indicator::None
            } else if session.status == SessionStatus::Working {
                Indicator::Working
            } else {
                Indicator::AwaitingInput
            }
        }
    }
}

/// Subagents running under a chat, staleness-checked like
/// [`effective_indicator`]: a session row nobody has refreshed inside
/// [`SESSION_STALE_MS`] belongs to a dead engine and counts nothing. Pure.
pub fn running_subagents(session: Option<&Session>, now: DateTime<Utc>) -> u32 {
    session
        .filter(|s| now.signed_duration_since(s.updated_at).num_milliseconds() <= SESSION_STALE_MS)
        .map_or(0, |s| s.running_subagents)
}

/// The full display status for a chat row or tab dot.
///
/// Live states win, then the seen marker decides completed or idle. Staleness gating rides on
/// [`effective_indicator`]; the derivation itself is [`crate::chat_indicator`].
pub fn display_status(chat: &Chat, session: Option<&Session>, now: DateTime<Utc>) -> ChatIndicator {
    let live = session.filter(|s| effective_indicator(Some(s), now) != Indicator::None);
    crate::chat_indicator(chat, live)
}

// ---------------------------------------------------------------------------
// Sort orders
// ---------------------------------------------------------------------------

/// Active-list order: pure recency, `last_message_at` desc with `created_at` as fallback.
///
/// The id breaks ties so the sort is total. Never by attention: status drives the dot, never
/// the position, so opening a finished session (which marks it seen) never moves its row from
/// under the pointer.
pub fn sort_active(rows: &mut [(ChatIndicator, &Chat)]) {
    rows.sort_by(|(_, a), (_, b)| {
        let ka = a.last_message_at.unwrap_or(a.created_at);
        let kb = b.last_message_at.unwrap_or(b.created_at);
        kb.cmp(&ka).then_with(|| a.id.cmp(&b.id))
    });
}

/// Session-tab order for a space: creation order (activity never reorders
/// tabs), id tiebreak. Pure.
pub fn sort_tabs(chats: &mut [&Chat]) {
    chats.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
}

/// Spaces list order: creation order, id tiebreak — total and stable across
/// devices. Pure.
pub fn sort_spaces(spaces: &mut [Space]) {
    spaces.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
}

/// The project a space belongs to.
///
/// Spaces sharing a repository identity (clones and worktrees of one repository, on any
/// device) are one project. A space without an identity is a project of its own. Pure.
pub fn project_key(space: &Space) -> String {
    match space.repository_id.as_deref() {
        Some(repository_id) => format!("repo:{repository_id}"),
        None => space.id.clone(),
    }
}

/// The space that speaks for `space`'s whole project: the oldest member, id tiebreak.
///
/// Its name and color stand for every member, and the choice is the same on every device, so
/// a repository looks alike everywhere. Pure.
pub fn representative_space<'a>(spaces: &'a [Space], space: &'a Space) -> &'a Space {
    let Some(repository_id) = space.repository_id.as_deref() else {
        return space;
    };
    spaces
        .iter()
        .filter(|s| s.repository_id.as_deref() == Some(repository_id))
        .min_by_key(|s| (s.created_at, s.id.as_str()))
        .unwrap_or(space)
}

/// Sidebar order: `last_message_at` desc, falling back to `created_at`; ties
/// break by `created_at` desc then id so the sort is total and stable across
/// devices. Pure.
pub fn sort_chats(chats: &mut [Chat]) {
    chats.sort_by(|a, b| {
        let ka = a.last_message_at.unwrap_or(a.created_at);
        let kb = b.last_message_at.unwrap_or(b.created_at);
        kb.cmp(&ka).then_with(|| b.created_at.cmp(&a.created_at)).then_with(|| a.id.cmp(&b.id))
    });
}

// ---------------------------------------------------------------------------
// Boot gate
// ---------------------------------------------------------------------------

/// The app gate's phase: what the window shows before the shell. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatePhase {
    /// Booting / probing — splash covers this.
    Loading,
    /// Engine unreachable and embedding failed.
    Failed(String),
    /// Render the shell.
    Ready,
}

/// The gate's phase while the view model's connection is at `connection`.
pub fn gate_phase(connection: &ConnectionStatus) -> GatePhase {
    match connection {
        ConnectionStatus::Connecting => GatePhase::Loading,
        ConnectionStatus::Failed(err) => GatePhase::Failed(err.clone()),
        ConnectionStatus::Ready => GatePhase::Ready,
    }
}

#[cfg(test)]
mod project_tests {
    use super::*;

    fn space(id: &str, device: &str, repository: Option<&str>, minutes: i64) -> Space {
        Space {
            id: id.into(),
            device_id: device.into(),
            path: format!("/{device}/{id}"),
            name: None,
            git_detected: repository.is_some(),
            git_checked_at: None,
            checkout_id: None,
            repository_id: repository.map(str::to_owned),
            created_at: DateTime::<Utc>::UNIX_EPOCH
                .checked_add_signed(chrono::TimeDelta::minutes(minutes))
                .unwrap(),
        }
    }

    #[test]
    fn spaces_sharing_a_repository_are_one_project_led_by_the_oldest() {
        let spaces = vec![
            space("laptop", "mac", Some("github.com/o/r"), 2),
            space("server", "vps", Some("github.com/o/r"), 1),
            space("notes", "mac", None, 0),
        ];
        assert_eq!(project_key(&spaces[0]), "repo:github.com/o/r");
        assert_eq!(project_key(&spaces[0]), project_key(&spaces[1]));
        assert_eq!(project_key(&spaces[2]), "notes");
        assert_eq!(representative_space(&spaces, &spaces[0]).id, "server");
        assert_eq!(representative_space(&spaces, &spaces[1]).id, "server");
        assert_eq!(representative_space(&spaces, &spaces[2]).id, "notes");
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn running_session(running: u32, updated_at: DateTime<Utc>) -> Session {
        Session {
            last_completed_turn: None,
            chat_id: "chat".into(),
            device_id: "dev".into(),
            status: SessionStatus::Working,
            started_at: None,
            updated_at,
            running_subagents: running,
            status_since: None,
            waiting_on: None,
        }
    }

    #[test]
    fn running_subagents_count_only_while_the_session_row_is_fresh() {
        let now = Utc::now();
        let fresh = running_session(3, now - chrono::Duration::seconds(10));
        assert_eq!(running_subagents(Some(&fresh), now), 3);
        // A crashed engine's row must not badge a chat forever.
        let stale = running_session(3, now - chrono::Duration::milliseconds(SESSION_STALE_MS + 1));
        assert_eq!(running_subagents(Some(&stale), now), 0);
        assert_eq!(running_subagents(None, now), 0);
    }
}

/// Parse an `AuthStatus` frame tolerantly.
///
/// Both spellings read: the engine's own enum (`{"_tag": "SignedIn", ...}`) and the proto
/// type's `{"state": "signedIn", ...}`.
pub fn parse_auth_state(value: &serde_json::Value) -> Option<AuthState> {
    if let Ok(state) = serde_json::from_value::<AuthState>(value.clone()) {
        return Some(state);
    }
    let tag = value.get("_tag").and_then(serde_json::Value::as_str)?;
    let user = || -> Option<crate::UserProfile> {
        let u = value.get("user")?;
        Some(crate::UserProfile {
            id: u.get("id")?.as_str()?.to_owned(),
            email: u.get("email")?.as_str()?.to_owned(),
            name: u.get("name").and_then(serde_json::Value::as_str).map(str::to_owned),
        })
    };
    match tag {
        "SignedOut" => Some(AuthState::SignedOut),
        "NeedsOrganization" => Some(AuthState::NeedsOrganization { user: user()? }),
        "SignedIn" => Some(AuthState::SignedIn {
            user: user()?,
            org_id: value.get("orgId").and_then(serde_json::Value::as_str).map(str::to_owned),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Sidebar grouping
// ---------------------------------------------------------------------------

/// One grouped-by-project sidebar section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGroup<'a> {
    /// The project's label, from [`project_label`].
    pub label: String,
    /// Its chats, most recent first.
    pub chats: Vec<&'a Chat>,
}

/// Project label for a chat: the basename of its cwd, or "No project".
pub fn project_label(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.map(str::trim).filter(|c| !c.is_empty()) else {
        return "No project".to_owned();
    };
    if crate::projectless::is_projectless_cwd(Some(cwd)) {
        return "No project".to_owned();
    }
    std::path::Path::new(cwd.trim_end_matches(['/', '\\']))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| cwd.to_owned())
}

/// Group chats by project label, preserving the incoming (recency) order both
/// for groups (by their most recent chat) and rows within a group. Pure.
pub fn group_chats<'a>(chats: impl IntoIterator<Item = &'a Chat>) -> Vec<ChatGroup<'a>> {
    let mut groups: Vec<ChatGroup<'a>> = Vec::new();
    for chat in chats {
        let label = project_label(chat.cwd.as_deref());
        match groups.iter_mut().find(|g| g.label == label) {
            Some(group) => group.chats.push(chat),
            None => groups.push(ChatGroup { label, chats: vec![chat] }),
        }
    }
    groups
}

/// Compact relative time ("now", "5m", "3h", "2d", "1w", …) — no "ago" suffix.
pub fn format_time_ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(then).num_seconds().max(0);
    // Under a minute reads as "now" — otherwise 45–59s floors to a bare "0m".
    if secs < 60 {
        return "now".to_owned();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    let weeks = days / 7;
    if weeks < 5 {
        return format!("{weeks}w");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo");
    }
    format!("{}y", days / 365)
}

/// Session-row sub-line, "project · branch": the repo
/// checkout identity. Either part may be missing; empty when both are. A
/// projectless chat names no project and no folder.
pub fn chat_location(chat: &Chat) -> Option<String> {
    let project = chat
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|c| !crate::projectless::is_projectless_cwd(Some(c)))
        .map(|c| project_label(Some(c)));
    let reference = chat.branch.as_deref().map(str::trim).filter(|b| !b.is_empty());
    match (project, reference) {
        (Some(p), Some(r)) => Some(format!("{p} · {r}")),
        (Some(p), None) => Some(p),
        (None, Some(r)) => Some(r.to_owned()),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// Tool summaries (pure)
// ---------------------------------------------------------------------------

/// Collapse model-generated text onto ONE line for single-line surfaces (tool
/// chips, titles, previews): newlines, tabs and runs of whitespace become
/// single spaces, trimmed.
///
/// Both viewports need this for the same reason from opposite directions — gpui
/// breaks on a literal `\n` before its ellipsis logic, and a terminal cell grid
/// would take an embedded newline as a cursor move.
pub fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Per-kind chip label + one-line detail, one derivation for every surface that names a
/// tool.
pub fn tool_chip_content(call: &crate::ToolCall) -> (&'static str, String) {
    let (label, detail) = tool_chip_content_raw(call);
    (label, single_line(&detail))
}

fn tool_chip_content_raw(call: &crate::ToolCall) -> (&'static str, String) {
    use crate::ToolCall;
    match call {
        ToolCall::Exec { command } => ("Run", command.clone()),
        ToolCall::ReadFile { path } => ("Read", path.clone()),
        ToolCall::WriteFile { path, .. } => ("Write", path.clone()),
        ToolCall::EditFile { path, .. } => ("Edit", path.clone()),
        ToolCall::ApplyPatch { path } => {
            ("Patch", path.clone().unwrap_or_else(|| "workspace".into()))
        }
        ToolCall::Search { pattern, path } => (
            "Search",
            match path {
                Some(path) => format!("{pattern} in {path}"),
                None => pattern.clone(),
            },
        ),
        ToolCall::Glob { pattern } => ("Glob", pattern.clone()),
        ToolCall::WebFetch { url, .. } => ("Fetch", url.clone()),
        ToolCall::WebSearch { query } => ("Web", query.clone()),
        ToolCall::Todo { items } => {
            let done = items.iter().filter(|i| i.done).count();
            ("Todo", format!("{done}/{} done", items.len()))
        }
        ToolCall::Mcp { server, tool, .. } => ("MCP", format!("{server} · {tool}")),
        // Subagent spawns decode as Unknown named "Agent[: <description>]"
        // (every native driver's convention): label them "Agent" with the
        // description as the detail — "Tool · Agent: scan repo" read as two
        // labels fighting.
        ToolCall::Unknown { name, .. } => match name.strip_prefix("Agent: ") {
            Some(description) => ("Agent", description.to_owned()),
            None if name == "Agent" => ("Agent", String::new()),
            None => ("Tool", name.clone()),
        },
    }
}

/// The status-dot palette, as oklch triples (L, C, H°).
///
/// Colors live here rather than in the viewport because the *meaning* of a
/// dot is part of the protocol, not the presentation — a given status must
/// read the same on every surface. `slopty-shell` has the oklch→sRGB math.
pub mod dot {
    /// Running. Pink, not amber: the harsh yellow read as a warning, and running
    /// is routine.
    pub const WORKING: (f32, f32, f32) = (0.718, 0.202, 349.761);
    /// Asking a question. Indigo — must read differently from "busy" at a glance.
    pub const AWAITING: (f32, f32, f32) = (0.673, 0.182, 276.935);
    /// Errored. Red-400.
    pub const ERRORED: (f32, f32, f32) = (0.704, 0.191, 22.216);
    /// Finished but unseen. Emerald — reads as "ready for you".
    pub const COMPLETED: (f32, f32, f32) = (0.765, 0.177, 163.223);
}
