//! Coding-agent tracking on the host.
//!
//! Claude Code reports what it is doing through hooks: for every registered event it runs a
//! command with a JSON description on stdin. Slopty registers `slopty hook` for the events it
//! cares about; the CLI forwards the JSON, together with the `SLOPTY_SESSION` the terminal was
//! spawned with, to `slopty-hostd`, which feeds it to an [`AgentTable`]. The table keeps one
//! [`Tracker`] per terminal session and turns the raw hook stream into
//! [`AgentStatus`] transitions that the daemon broadcasts as [`AgentEvent`]s.
//!
//! The hook payload is parsed leniently ([`Hook`]): unknown events and unknown fields are
//! ignored, so a newer Claude Code never breaks the relay.

use std::collections::HashMap;

use serde::Deserialize;
use slopty_core::SessionId;
use slopty_proto::agent::{AgentEvent, AgentKind, AgentStatus, BlockReason};

/// Hook events `slopty hook install` registers. The relay ignores everything else.
pub const HOOK_EVENTS: [&str; 12] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionDenied",
    "Notification",
    "Elicitation",
    "ElicitationResult",
    "Stop",
];

/// Longest `detail` string sent to clients.
const DETAIL_MAX: usize = 60;

/// A Claude Code hook payload, the fields Slopty reads.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Hook {
    /// Claude Code's session id.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The event (`hook_event_name`).
    #[serde(default, rename = "hook_event_name")]
    pub event: String,
    /// Tool events.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Tool arguments.
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    /// `Notification`.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// `Notification`.
    #[serde(default)]
    pub message: Option<String>,
    /// `UserPromptSubmit`.
    #[serde(default)]
    pub prompt: Option<String>,
    /// `SessionStart`: `startup|resume|clear|compact|fork`.
    #[serde(default)]
    pub source: Option<String>,
}

impl Hook {
    /// Parse a payload; never fails on unknown shapes, only on non-JSON.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// One line describing the tool call ("Bash: cargo test", "Edit src/main.rs").
    fn tool_detail(&self) -> Option<String> {
        let tool = self.tool_name.as_deref()?;
        let input = self.tool_input.as_ref();
        let arg = |key: &str| input.and_then(|v| v.get(key)).and_then(|v| v.as_str());
        let text = match tool {
            "Bash" => arg("command").map(|c| format!("$ {}", first_line(c))),
            "Edit" | "Write" | "Read" | "NotebookEdit" => {
                arg("file_path").map(|p| format!("{tool} {}", short_path(p)))
            }
            "Glob" | "Grep" => arg("pattern").map(|p| format!("{tool} {p}")),
            "Agent" | "Task" => arg("description").map(|d| format!("Agent: {d}")),
            "WebFetch" => arg("url").map(|u| format!("Fetch {u}")),
            "WebSearch" => arg("query").map(|q| format!("Search {q}")),
            "Skill" => arg("skill").map(|s| format!("/{s}")),
            _ => None,
        };
        Some(truncate(&text.unwrap_or_else(|| tool.to_owned())))
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// Last two path components.
fn short_path(p: &str) -> String {
    let mut parts: Vec<&str> = p.rsplit('/').take(2).collect();
    parts.reverse();
    parts.join("/")
}

fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= DETAIL_MAX {
        return s.to_owned();
    }
    let cut: String = s.chars().take(DETAIL_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Tool name from "Claude needs your permission to use Bash"-style messages.
fn tool_from_message(message: Option<&str>) -> Option<String> {
    let (_before, after) = message?.split_once("to use ")?;
    let tool = after.split(|c: char| c.is_whitespace() || c == '.').next()?;
    (!tool.is_empty()).then(|| tool.to_owned())
}

/// Per-session agent state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracker {
    status: AgentStatus,
    agent_session: Option<String>,
    detail: Option<String>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self { status: AgentStatus::None, agent_session: None, detail: None }
    }
}

impl Tracker {
    /// Current status.
    #[must_use]
    pub const fn status(&self) -> &AgentStatus {
        &self.status
    }

    /// The event describing the current state (for a joining client).
    #[must_use]
    pub fn event(&self, session: SessionId) -> AgentEvent {
        AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status: self.status.clone(),
            agent_session: self.agent_session.clone(),
            detail: self.detail.clone(),
            attention: false,
        }
    }

    /// Apply one hook; the resulting event when the visible state changed.
    pub fn apply(&mut self, session: SessionId, hook: &Hook) -> Option<AgentEvent> {
        if hook.session_id.is_some() {
            self.agent_session.clone_from(&hook.session_id);
        }
        let (status, detail) = Self::next(hook)?;
        let was_blocked = matches!(self.status, AgentStatus::Blocked(_));
        let attention = match status {
            AgentStatus::Blocked(_) => !was_blocked,
            AgentStatus::Done => true,
            _ => false,
        };
        if status == self.status && detail == self.detail {
            return None;
        }
        self.status = status;
        self.detail = detail;
        if self.status == AgentStatus::None {
            self.agent_session = None;
        }
        Some(AgentEvent { attention, ..self.event(session) })
    }

    /// The transition for a hook, if it means anything to us.
    fn next(hook: &Hook) -> Option<(AgentStatus, Option<String>)> {
        let tool = || hook.tool_name.clone().unwrap_or_default();
        Some(match hook.event.as_str() {
            "SessionStart" => (AgentStatus::Idle, None),
            "SessionEnd" => (AgentStatus::None, None),
            "UserPromptSubmit" => {
                (AgentStatus::Working, hook.prompt.as_deref().map(first_line).map(truncate))
            }
            "PreToolUse" if hook.tool_name.as_deref() == Some("AskUserQuestion") => {
                (AgentStatus::Blocked(BlockReason::Question), None)
            }
            "PreToolUse" => (AgentStatus::Tool { tool: tool() }, hook.tool_detail()),
            "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" | "ElicitationResult" => {
                (AgentStatus::Working, None)
            }
            "PermissionRequest" => {
                (AgentStatus::Blocked(BlockReason::Permission { tool: tool() }), hook.tool_detail())
            }
            "Elicitation" => (AgentStatus::Blocked(BlockReason::Elicitation), None),
            "Notification" => match hook.notification_type.as_deref()? {
                "permission_prompt" => {
                    let tool = tool_from_message(hook.message.as_deref()).unwrap_or_default();
                    (AgentStatus::Blocked(BlockReason::Permission { tool }), None)
                }
                "idle_prompt" => (AgentStatus::Blocked(BlockReason::IdlePrompt), None),
                "agent_needs_input" => (AgentStatus::Blocked(BlockReason::Question), None),
                "elicitation_dialog" | "elicitation_url_dialog" => {
                    (AgentStatus::Blocked(BlockReason::Elicitation), None)
                }
                "elicitation_complete" | "elicitation_response" => (AgentStatus::Working, None),
                _ => return None,
            },
            "Stop" => (AgentStatus::Done, None),
            _ => return None,
        })
    }
}

/// All sessions' agents.
#[derive(Debug, Default)]
pub struct AgentTable {
    sessions: HashMap<SessionId, Tracker>,
}

impl AgentTable {
    /// Feed a hook for a session.
    pub fn apply(&mut self, session: SessionId, hook: &Hook) -> Option<AgentEvent> {
        let tracker = self.sessions.entry(session).or_default();
        let event = tracker.apply(session, hook);
        if tracker.status() == &AgentStatus::None {
            self.sessions.remove(&session);
        }
        event
    }

    /// The session's terminal went away.
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
    }

    /// Current state of every session with an agent, for a joining client.
    #[must_use]
    pub fn snapshot(&self) -> Vec<AgentEvent> {
        self.sessions.iter().map(|(id, t)| t.event(*id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(json: &str) -> Hook {
        Hook::parse(json).expect("hook json")
    }

    #[test]
    fn a_turn_walks_idle_working_tool_done() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"session_id":"abc","hook_event_name":"SessionStart","source":"startup"}"#,
                ),
            )
            .expect("start");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(e.agent_session.as_deref(), Some("abc"));
        assert!(!e.attention);

        let e = t
            .apply(
                sid,
                &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"fix the build\nplease"}"#),
            )
            .expect("prompt");
        assert_eq!(e.status, AgentStatus::Working);
        assert_eq!(e.detail.as_deref(), Some("fix the build"));

        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test\n"},"tool_use_id":"t1"}"#,
                ),
            )
            .expect("tool");
        assert_eq!(e.status, AgentStatus::Tool { tool: "Bash".into() });
        assert_eq!(e.detail.as_deref(), Some("$ cargo test"));

        let e = t
            .apply(
                sid,
                &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"t1"}"#),
            )
            .expect("post");
        assert_eq!(e.status, AgentStatus::Working);
        assert_eq!(e.detail, None);

        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#))
            .expect("stop");
        assert_eq!(e.status, AgentStatus::Done);
        assert!(e.attention);

        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"SessionEnd","end_reason":"other"}"#))
            .expect("end");
        assert_eq!(e.status, AgentStatus::None);
        assert_eq!(e.agent_session, None);
    }

    #[test]
    fn permission_blocks_once_with_attention() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/a/b/src/main.rs"}}"#,
                ),
            )
            .expect("permission");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Permission { tool: "Edit".into() }));
        assert_eq!(e.detail.as_deref(), Some("Edit src/main.rs"));
        assert!(e.attention);

        // The notification for the same prompt is a quiet correction, not a second alert.
        let e = t.apply(
            sid,
            &hook(
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Edit"}"#,
            ),
        );
        assert!(e.is_none_or(|e| !e.attention));

        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Edit"}"#))
            .expect("resumes");
        assert_eq!(e.status, AgentStatus::Working);
        assert!(!e.attention);
    }

    #[test]
    fn questions_and_idle_prompts_block() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{}}"#))
            .expect("question");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"waiting"}"#))
            .expect("idle");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::IdlePrompt));
        assert!(!e.attention, "already blocked: no second alert");
    }

    #[test]
    fn unknown_events_and_duplicates_are_silent() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        assert!(
            t.apply(sid, &hook(r#"{"hook_event_name":"PreCompact","trigger_type":"auto"}"#))
                .is_none()
        );
        assert!(
            t.apply(
                sid,
                &hook(r#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#)
            )
            .is_none()
        );
        assert!(t.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).is_some());
        assert!(t.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).is_none());
        assert!(t.apply(sid, &hook(r#"{"unrelated":true}"#)).is_none());
    }

    #[test]
    fn table_snapshots_live_sessions_and_drops_ended_ones() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        table.apply(a, &hook(r#"{"session_id":"x","hook_event_name":"SessionStart"}"#));
        table.apply(b, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#));
        assert_eq!(table.snapshot().len(), 2);
        table.apply(a, &hook(r#"{"hook_event_name":"SessionEnd"}"#));
        assert_eq!(table.snapshot().len(), 1);
        table.forget(b);
        assert!(table.snapshot().is_empty());
    }

    #[test]
    fn details_are_short() {
        let long = "x".repeat(200);
        let h = hook(&format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"{long}"}}}}"#
        ));
        let d = h.tool_detail().expect("detail");
        assert!(d.chars().count() <= DETAIL_MAX);
        assert!(d.ends_with('…'));
        assert_eq!(
            tool_from_message(Some("Claude needs your permission to use Bash")).as_deref(),
            Some("Bash")
        );
        assert_eq!(short_path("/Users/x/proj/src/lib.rs"), "src/lib.rs");
    }
}
