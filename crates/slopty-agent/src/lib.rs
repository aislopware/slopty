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
//!
//! When the agent stops or waits on the human, the event's `detail` says what it wants: the
//! question it asked, the elicitation's message, or (on `Stop`) the last line it said, from the
//! payload's `last_assistant_message`; when a payload carries none of those but names a
//! transcript, the daemon reads the transcript tail ([`transcript`]) and fills the detail in.
//!
//! Hooks are only the strongest of four signals. A `claude` the human started by hand in any
//! Slopty terminal — or one running before `slopty hook install` — is attributed from what the
//! host can see anyway: its [`detect`]ed foreground process, the [`title`] it paints, and the
//! JSONL transcript [`discover`]ed from its working directory. [`Tracker::observe`] merges
//! them in [`AgentSource`] order, so a weaker signal never overwrites what a stronger one
//! said and hooks stay authoritative once they speak.
//!
//! An agent Slopty starts itself is not observed at all: it is driven over Claude Code's
//! stream-json protocol ([`stream`]), which reports every record and permission directly.

#![forbid(unsafe_code)]

pub mod detect;
pub mod discover;
pub mod files;
pub mod hooks;
pub mod stream;
pub mod title;
pub mod transcript;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use slopty_core::SessionId;
use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason};

use crate::detect::Program;
use crate::title::TitleSignal;
use crate::transcript::Progress;

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
pub const DETAIL_MAX: usize = 60;

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
    /// The conversation transcript (JSONL); on every event in practice.
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// `Stop`: the text of the turn's final response, so nobody has to read the transcript.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
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

    /// The first question of an `AskUserQuestion` call.
    fn question(&self) -> Option<String> {
        let question = self
            .tool_input
            .as_ref()?
            .get("questions")?
            .as_array()?
            .first()?
            .get("question")?
            .as_str()?;
        Some(truncate(question))
    }

    /// The last line the assistant said, from a `Stop` payload.
    fn last_said(&self) -> Option<String> {
        transcript::last_line(self.last_assistant_message.as_deref()?).map(|l| truncate(&l))
    }
}

/// Whether an event's `detail` should be recovered from the transcript when the payload gave
/// none: the states where the badge is read as "what does it want?".
#[must_use]
pub const fn wants_transcript(event: &AgentEvent) -> bool {
    event.detail.is_none()
        && matches!(
            event.status,
            AgentStatus::Blocked(BlockReason::Question | BlockReason::Elicitation)
                | AgentStatus::Done
        )
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

/// Clip to [`DETAIL_MAX`] characters with an ellipsis.
#[must_use]
pub fn truncate(s: &str) -> String {
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

/// What the host can see of a session without any help from the agent: the program in the
/// foreground of its tty, the title that program paints, and the directory it runs in.
///
/// Everything here is a fact about the terminal, gathered by the host on a timer; nothing in
/// it requires the agent to cooperate. A `program` of `None` means the platform would not say
/// (the lookup failed), which is not the same as "no agent runs here" and never clears one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// The foreground process of the session's tty.
    pub program: Option<Program>,
    /// The terminal title (OSC 0/2), when the session set one.
    pub title: Option<String>,
    /// Where the agent runs: the foreground process's own working directory, else OSC 7.
    pub cwd: Option<PathBuf>,
    /// The foreground process's pid, when the platform said.
    pub pid: Option<i32>,
    /// When that process started, when the platform said. Together with `pid` this is the
    /// identity of the process: one `claude` replaced by another inside a tick is not the same
    /// agent, whatever the session it runs in.
    pub started: Option<SystemTime>,
}

/// Where to look for a session's transcript, and which file is being read now.
///
/// Claude Code starts a new file whenever the human runs `/clear` or resumes another
/// conversation, so a session that already has a `current` file still has to be looked up
/// again: the newest file in the project directory is the one it writes now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    /// The terminal session.
    pub session: SessionId,
    /// The directory the agent runs in ([`discover::project_dir`]).
    pub cwd: PathBuf,
    /// When the agent process was first seen; older files are other conversations.
    pub since: SystemTime,
    /// The transcript being read now, when one has been found already.
    pub current: Option<String>,
}

/// Consecutive probes without the agent in the foreground before a *hooked* session is ended
/// by the process table alone. The hook relay (`slopty hook`) is itself briefly the foreground
/// process of the terminal it reports on, so one probe never means the agent is gone.
const ABSENT_BEFORE_GONE: u8 = 4;

/// Per-session agent state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracker {
    status: AgentStatus,
    agent_session: Option<String>,
    detail: Option<String>,
    /// The conversation file, from the last hook that named one or from [`discover`].
    transcript_path: Option<String>,
    /// Which signal the current status came from.
    source: AgentSource,
    /// A hook has spoken for this session: weaker signals stop deciding the status, and the
    /// foreground process no longer clears the agent (the hooks say when it ends).
    hooked: bool,
    /// When an agent process was first seen here; the transcript is the newest file written
    /// at or after this.
    first_seen: Option<SystemTime>,
    /// The agent's working directory, for [`discover::transcript_for`].
    cwd: Option<PathBuf>,
    /// Consecutive probes whose foreground process was not the agent.
    absent: u8,
    /// The agent process this tracker follows (`pid`, start time), when the platform said.
    process: Option<(i32, Option<SystemTime>)>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            status: AgentStatus::None,
            agent_session: None,
            detail: None,
            transcript_path: None,
            source: AgentSource::Process,
            hooked: false,
            first_seen: None,
            cwd: None,
            absent: 0,
            process: None,
        }
    }
}

impl Tracker {
    /// Current status.
    #[must_use]
    pub const fn status(&self) -> &AgentStatus {
        &self.status
    }

    /// Which signal the current status came from.
    #[must_use]
    pub const fn source(&self) -> AgentSource {
        self.source
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
            source: self.source,
        }
    }

    /// Apply one hook; the resulting event when the visible state changed.
    pub fn apply(&mut self, session: SessionId, hook: &Hook) -> Option<AgentEvent> {
        self.hooked = true;
        if hook.session_id.is_some() {
            self.agent_session.clone_from(&hook.session_id);
        }
        if hook.transcript_path.is_some() {
            self.transcript_path.clone_from(&hook.transcript_path);
        }
        let (status, detail) = self.next(hook)?;
        let was_blocked = matches!(self.status, AgentStatus::Blocked(_));
        let attention = match status {
            AgentStatus::Blocked(_) => !was_blocked,
            AgentStatus::Done => true,
            _ => false,
        };
        if status == self.status && detail == self.detail && self.source == AgentSource::Hook {
            return None;
        }
        self.status = status;
        self.detail = detail;
        self.source = AgentSource::Hook;
        if self.status == AgentStatus::None {
            self.agent_session = None;
        }
        Some(AgentEvent { attention, ..self.event(session) })
    }

    /// Fold in what the host can see of the session (see [`Observation`]).
    ///
    /// A foreground process that is not an agent clears the session. When a hook has spoken,
    /// the hooks decide when it ends and this only steps in for an agent the host actually saw
    /// running that has now been gone for `ABSENT_BEFORE_GONE` probes — a `claude` killed
    /// without a `SessionEnd` would otherwise keep its pill until the terminal exits.
    /// Otherwise the title decides working from idle, and the mere presence of the process
    /// means [`AgentStatus`] `Idle`; neither ever overwrites what the transcript or a hook said.
    pub fn observe(&mut self, session: SessionId, obs: &Observation) -> Option<AgentEvent> {
        let Some(program) = obs.program.as_ref() else {
            // The platform would not say; that is not evidence of anything.
            return None;
        };
        if !program.is_claude() {
            if self.status == AgentStatus::None {
                return None;
            }
            if self.hooked {
                // Hooks arrive from a relay, not from the session's foreground process: only
                // an agent we watched run there, gone for several probes, ends this way.
                self.first_seen?;
                self.absent = self.absent.saturating_add(1);
                if self.absent < ABSENT_BEFORE_GONE {
                    return None;
                }
            }
            *self = Self::default();
            return Some(self.event(session));
        }
        // A `claude` replaced by another one between two probes is a different agent: its
        // conversation, its status and the hooks that spoke for the old one describe nothing
        // here any more, so the session is attributed again from scratch.
        if let Some(pid) = obs.pid {
            let process = (pid, obs.started);
            if self.process.is_some_and(|known| known != process) {
                *self = Self::default();
            }
            self.process = Some(process);
        }
        self.absent = 0;
        if self.first_seen.is_none() {
            self.first_seen = Some(obs.started.unwrap_or_else(SystemTime::now));
        }
        if obs.cwd.is_some() {
            self.cwd.clone_from(&obs.cwd);
        }
        if self.source >= AgentSource::Transcript {
            return None;
        }
        let (status, source) = match obs.title.as_deref().and_then(title::signal) {
            Some(TitleSignal::Working) => (AgentStatus::Working, AgentSource::Title),
            Some(TitleSignal::Idle) => (AgentStatus::Idle, AgentSource::Title),
            // No title to read: the process being there is all we know.
            None => (AgentStatus::Idle, AgentSource::Process),
        };
        self.set(session, status, None, source)
    }

    /// Fold in what the session's transcript says the turn is doing. Ignored once a hook has
    /// spoken: the hooks report blocking, which the transcript never does.
    pub fn observe_progress(
        &mut self,
        session: SessionId,
        progress: &Progress,
    ) -> Option<AgentEvent> {
        if self.hooked {
            return None;
        }
        self.set(session, progress.status.clone(), progress.detail.clone(), AgentSource::Transcript)
    }

    /// Move to a state a signal weaker than a hook reported; `None` when nothing visible
    /// changed.
    fn set(
        &mut self,
        session: SessionId,
        status: AgentStatus,
        detail: Option<String>,
        source: AgentSource,
    ) -> Option<AgentEvent> {
        if status == self.status && detail == self.detail && source == self.source {
            return None;
        }
        // Without hooks a finished turn is only news when we watched it run: a transcript read
        // for the first time is usually a conversation that ended hours ago.
        let attention = status == AgentStatus::Done
            && matches!(self.status, AgentStatus::Working | AgentStatus::Tool { .. });
        self.status = status;
        self.detail = detail;
        self.source = source;
        Some(AgentEvent { attention, ..self.event(session) })
    }

    /// Where to look for this agent's transcript, and the file being read now.
    ///
    /// An agent keeps asking after its file has been found: `/clear` and `/resume` start a new
    /// one, and the tail has to move with it. A hook names the file itself, so a hooked
    /// session only asks while nothing has named one.
    fn discovery(&self, session: SessionId) -> Option<Discovery> {
        if self.status == AgentStatus::None || (self.hooked && self.transcript_path.is_some()) {
            return None;
        }
        Some(Discovery {
            session,
            cwd: self.cwd.clone()?,
            since: self.first_seen?,
            current: self.transcript_path.clone(),
        })
    }

    /// Replace the detail (something recovered after the hook, such as a transcript line).
    /// Returns whether it changed.
    pub fn set_detail(&mut self, detail: &str) -> bool {
        let detail = Some(truncate(detail)).filter(|d| !d.is_empty());
        if detail == self.detail {
            return false;
        }
        self.detail = detail;
        true
    }

    /// The transition for a hook, if it means anything to us.
    fn next(&self, hook: &Hook) -> Option<(AgentStatus, Option<String>)> {
        let tool = || hook.tool_name.clone().unwrap_or_default();
        Some(match hook.event.as_str() {
            "SessionStart" => (AgentStatus::Idle, None),
            "SessionEnd" => (AgentStatus::None, None),
            "UserPromptSubmit" => {
                (AgentStatus::Working, hook.prompt.as_deref().map(first_line).map(truncate))
            }
            "PreToolUse" if hook.tool_name.as_deref() == Some("AskUserQuestion") => {
                (AgentStatus::Blocked(BlockReason::Question), hook.question())
            }
            "PreToolUse" => (AgentStatus::Tool { tool: tool() }, hook.tool_detail()),
            "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" | "ElicitationResult" => {
                (AgentStatus::Working, None)
            }
            "PermissionRequest" => {
                (AgentStatus::Blocked(BlockReason::Permission { tool: tool() }), hook.tool_detail())
            }
            "Elicitation" => (
                AgentStatus::Blocked(BlockReason::Elicitation),
                hook.message.as_deref().map(first_line).map(truncate),
            ),
            "Notification" => match hook.notification_type.as_deref()? {
                // Follows a `PermissionRequest` a few seconds later and, as of Claude Code
                // 2.1.261, says only "Claude needs your permission": when the request already
                // put the tool and its arguments on the badge, keep them.
                "permission_prompt" => match tool_from_message(hook.message.as_deref()) {
                    None if matches!(
                        self.status,
                        AgentStatus::Blocked(BlockReason::Permission { .. })
                    ) =>
                    {
                        return None;
                    }
                    tool => (
                        AgentStatus::Blocked(BlockReason::Permission {
                            tool: tool.unwrap_or_default(),
                        }),
                        None,
                    ),
                },
                "idle_prompt" => (AgentStatus::Blocked(BlockReason::IdlePrompt), None),
                "agent_needs_input" => (AgentStatus::Blocked(BlockReason::Question), None),
                "elicitation_dialog" | "elicitation_url_dialog" => {
                    (AgentStatus::Blocked(BlockReason::Elicitation), None)
                }
                "elicitation_complete" | "elicitation_response" => (AgentStatus::Working, None),
                _ => return None,
            },
            "Stop" => (AgentStatus::Done, hook.last_said()),
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

    /// Feed one round of what the host can see of a session ([`Tracker::observe`]).
    pub fn observe(&mut self, session: SessionId, obs: &Observation) -> Option<AgentEvent> {
        // A session with nothing agent-like in it must not grow an entry on every tick.
        if !self.sessions.contains_key(&session)
            && !obs.program.as_ref().is_some_and(Program::is_claude)
        {
            return None;
        }
        let tracker = self.sessions.entry(session).or_default();
        let event = tracker.observe(session, obs);
        if tracker.status() == &AgentStatus::None {
            self.sessions.remove(&session);
        }
        event
    }

    /// Feed what a session's transcript says the turn is doing
    /// ([`Tracker::observe_progress`]).
    pub fn observe_progress(
        &mut self,
        session: SessionId,
        progress: &Progress,
    ) -> Option<AgentEvent> {
        self.sessions.get_mut(&session)?.observe_progress(session, progress)
    }

    /// Every unhooked agent's transcript lookup: where to look, and what is being read now
    /// ([`Discovery`], [`discover::transcript_for`]).
    #[must_use]
    pub fn discoveries(&self) -> Vec<Discovery> {
        self.sessions.iter().filter_map(|(id, t)| t.discovery(*id)).collect()
    }

    /// Drop every agent whose session the host no longer runs, and say so: a session that
    /// went away takes its agent with it whatever its last signal said.
    pub fn retain(&mut self, live: &[SessionId]) -> Vec<AgentEvent> {
        let mut gone = Vec::new();
        self.sessions.retain(|session, _tracker| {
            if live.contains(session) {
                return true;
            }
            gone.push(Tracker::default().event(*session));
            false
        });
        gone
    }

    /// Whether an event still describes its session.
    ///
    /// The daemon computes events from a poll and broadcasts them; a hook arriving on the
    /// control socket in between has already told every client something newer, and the poll's
    /// event must not put the older state back (a permission badge would vanish until the next
    /// hook). Anything the table has moved past is dropped instead of sent.
    #[must_use]
    pub fn is_current(&self, event: &AgentEvent) -> bool {
        self.sessions.get(&event.session).map_or(event.status == AgentStatus::None, |tracker| {
            tracker.status == event.status && tracker.source == event.source
        })
    }

    /// Every session with an agent in it.
    #[must_use]
    pub fn sessions_with_agents(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    /// Record the transcript a session's agent writes, found without a hook. Returns whether
    /// the file moved: the caller reads a new conversation from its start.
    pub fn set_transcript_path(&mut self, session: SessionId, path: &Path) -> bool {
        let Some(tracker) = self.sessions.get_mut(&session) else { return false };
        let path = Some(path.to_string_lossy().into_owned());
        if tracker.transcript_path == path {
            return false;
        }
        tracker.transcript_path = path;
        true
    }

    /// Put a recovered detail on a session's agent (see [`wants_transcript`]); the next
    /// snapshot carries it. Returns whether anything changed.
    pub fn set_detail(&mut self, session: SessionId, detail: &str) -> bool {
        self.sessions.get_mut(&session).is_some_and(|t| t.set_detail(detail))
    }

    /// The session's terminal went away.
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
    }

    /// Where the session's conversation is written, once a hook has said.
    #[must_use]
    pub fn transcript_path(&self, session: SessionId) -> Option<PathBuf> {
        self.sessions.get(&session)?.transcript_path.as_deref().map(PathBuf::from)
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
    fn blocked_and_done_say_what_they_want() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which framework?","header":"Framework","options":[]}]}}"#,
                ),
            )
            .expect("question");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        assert_eq!(e.detail.as_deref(), Some("Which framework?"));
        assert!(!wants_transcript(&e), "the payload said it");

        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Elicitation","mcp_server_name":"x","message":"Please provide your credentials","mode":"form"}"#,
                ),
            )
            .expect("elicitation");
        assert_eq!(e.detail.as_deref(), Some("Please provide your credentials"));

        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"All green.\n\nDone. `opened-enter` was created."}"#,
                ),
            )
            .expect("stop");
        assert_eq!(e.status, AgentStatus::Done);
        assert_eq!(e.detail.as_deref(), Some("Done. `opened-enter` was created."));

        // An older Claude Code without `last_assistant_message`: the daemon asks the transcript.
        let mut t = Tracker::default();
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"Stop","transcript_path":"/x.jsonl"}"#))
            .expect("stop");
        assert_eq!(e.detail, None);
        assert!(wants_transcript(&e));
        assert!(t.set_detail("Running the tests now."));
        assert!(!t.set_detail("Running the tests now."));
        assert_eq!(t.event(sid).detail.as_deref(), Some("Running the tests now."));
    }

    #[test]
    fn a_bare_permission_notification_keeps_the_request_detail() {
        // Observed with Claude Code 2.1.261: the notification that follows the request says
        // only "Claude needs your permission".
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch x"}}"#,
                ),
            )
            .expect("permission");
        assert_eq!(e.detail.as_deref(), Some("$ touch x"));
        let e = t.apply(
            sid,
            &hook(
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission"}"#,
            ),
        );
        assert_eq!(e, None, "nothing to add: the request already said it all");
        assert_eq!(
            t.status(),
            &AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() })
        );
        // Without a preceding request the notification still blocks, tool unknown.
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission"}"#,
                ),
            )
            .expect("blocks");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Permission { tool: String::new() }));
    }

    /// What the host sees of a terminal running `program` with `title`, always the same
    /// process ([`process`] for another one).
    fn seen(program: &str, title: Option<&str>) -> Observation {
        process(program, title, 4321)
    }

    /// [`seen`] for a named process: `pid`, started a second per pid after the epoch.
    fn process(program: &str, title: Option<&str>, pid: i32) -> Observation {
        Observation {
            program: Some(Program::named(program)),
            title: title.map(str::to_owned),
            cwd: Some(PathBuf::from("/tmp/project")),
            pid: Some(pid),
            started: Some(now()),
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(1_000_000))
            .expect("a time in range")
    }

    #[test]
    fn a_hand_started_claude_is_attributed_from_the_process_and_the_title() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        // A shell is not an agent, and an empty tracker has nothing to clear.
        assert_eq!(t.observe(sid, &seen("zsh", Some("~/project"))), None);

        // The process alone: the agent is there, idle, and the pill can be drawn.
        let e = t.observe(sid, &seen("claude", None)).expect("the process");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(e.source, AgentSource::Process);
        assert!(!e.attention);
        assert_eq!(t.observe(sid, &seen("claude", None)), None, "nothing changed");

        // Its title says a turn is running; the sparkle and the summary say it ended.
        let e = t.observe(sid, &seen("claude", Some("◐ Claude Code"))).expect("the title");
        assert_eq!((e.status, e.source), (AgentStatus::Working, AgentSource::Title));
        let e = t.observe(sid, &seen("claude", Some("✳ fix the build"))).expect("back to idle");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Title));

        // The transcript outranks the title and says what the turn is doing.
        let tool = Progress {
            status: AgentStatus::Tool { tool: "Bash".to_owned() },
            detail: Some("cargo test".to_owned()),
        };
        let e = t.observe_progress(sid, &tool).expect("the transcript");
        assert_eq!(e.status, AgentStatus::Tool { tool: "Bash".to_owned() });
        assert_eq!(e.source, AgentSource::Transcript);
        assert_eq!(e.detail.as_deref(), Some("cargo test"));
        assert_eq!(
            t.observe(sid, &seen("claude", Some("claude"))),
            None,
            "a title never overwrites the transcript"
        );

        // A finished turn we watched run is worth the human's attention.
        let done = Progress { status: AgentStatus::Done, detail: Some("Fixed.".to_owned()) };
        let e = t.observe_progress(sid, &done).expect("done");
        assert!(e.attention);

        // The process goes away: so does the agent.
        let e = t.observe(sid, &seen("zsh", Some("~/project"))).expect("gone");
        assert_eq!(e.status, AgentStatus::None);
    }

    #[test]
    fn a_transcript_read_for_the_first_time_is_not_an_alert() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", None)).expect("the process");
        let done = Progress { status: AgentStatus::Done, detail: Some("Hours ago.".to_owned()) };
        let e = t.observe_progress(sid, &done).expect("done");
        assert_eq!(e.status, AgentStatus::Done);
        assert!(!e.attention, "the turn ended before we ever looked");
    }

    #[test]
    fn hooks_outrank_everything_and_decide_when_the_agent_ends() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", Some("◐ Claude Code"))).expect("the title");
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch x"}}"#,
                ),
            )
            .expect("the hook");
        assert_eq!(e.source, AgentSource::Hook);
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }));

        // Neither weaker signal may talk over a blocked agent, and one probe without it in the
        // foreground may not end it: the hooks are relayed by a process that is itself briefly
        // the session's foreground one.
        assert_eq!(t.observe(sid, &seen("claude", Some("✳ touch x"))), None);
        let progress = Progress { status: AgentStatus::Working, detail: None };
        assert_eq!(t.observe_progress(sid, &progress), None);
        assert_eq!(t.observe(sid, &seen("slopty", Some("~/project"))), None);
        assert_eq!(
            t.status(),
            &AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() })
        );
    }

    #[test]
    fn a_hooked_agent_ends_when_the_process_it_ran_in_stays_gone() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", None)).expect("the process");
        t.apply(sid, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#))
            .expect("the hook");

        // `claude` killed with no `SessionEnd`: the pill stays through the probes a hook relay
        // could account for, then goes.
        let shell = seen("zsh", Some("~/project"));
        for probe in 1..ABSENT_BEFORE_GONE {
            assert_eq!(t.observe(sid, &shell), None, "probe {probe}");
        }
        let e = t.observe(sid, &shell).expect("gone");
        assert_eq!(e.status, AgentStatus::None);

        // An agent that only ever spoke through hooks is never ended this way: nothing of it
        // was ever in the foreground to disappear.
        let mut t = Tracker::default();
        t.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).expect("the hook");
        for _probe in 0..ABSENT_BEFORE_GONE.saturating_mul(2) {
            assert_eq!(t.observe(sid, &shell), None);
        }
        assert_eq!(t.status(), &AgentStatus::Idle);
    }

    #[test]
    fn a_second_claude_in_the_same_terminal_is_a_new_agent() {
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(sid, &process("claude", None, 100)).expect("the first agent");
        table.apply(sid, &hook(r#"{"session_id":"one","hook_event_name":"UserPromptSubmit"}"#));
        table.set_transcript_path(sid, Path::new("/tmp/one.jsonl"));

        // Between two probes the first `claude` exited and a second one started: nothing the
        // first said — its conversation, its status, its hooks — is about this one.
        let e = table.observe(sid, &process("claude", None, 101)).expect("the second agent");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Process));
        assert_eq!(table.transcript_path(sid), None, "the old conversation is not this one's");
        assert_eq!(
            table.discoveries().first().and_then(|d| d.current.clone()),
            None,
            "and it is looked up again"
        );
        // The same process seen again changes nothing.
        assert_eq!(table.observe(sid, &process("claude", None, 101)), None);
    }

    #[test]
    fn an_event_a_hook_has_overtaken_is_not_current_any_more() {
        // The daemon computes poll events, then reads files; a hook can arrive in between and
        // has already told every client something newer. `is_current` is what stops the poll's
        // event from putting the older state back.
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        let stale = table.observe(sid, &seen("claude", None)).expect("the process");
        assert!(table.is_current(&stale));
        table
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#,
                ),
            )
            .expect("the hook");
        assert!(!table.is_current(&stale), "the hook spoke after the poll was computed");

        // An event that says the agent ended is current exactly while there is no agent.
        let gone = Tracker::default().event(sid);
        assert!(!table.is_current(&gone));
        table.forget(sid);
        assert!(table.is_current(&gone));
    }

    #[test]
    fn a_lookup_that_says_nothing_changes_nothing() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", None)).expect("the process");
        let blind = Observation::default();
        assert_eq!(t.observe(sid, &blind), None);
        assert_eq!(t.status(), &AgentStatus::Idle);
    }

    #[test]
    fn the_table_asks_for_a_transcript_only_for_agents() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(a, &seen("claude", None));
        table.observe(b, &seen("zsh", None));
        assert_eq!(table.snapshot().len(), 1, "only the agent has an entry");
        assert_eq!(
            table.discoveries(),
            vec![Discovery {
                session: a,
                cwd: PathBuf::from("/tmp/project"),
                since: now(),
                current: None,
            }]
        );
        assert!(table.set_transcript_path(a, Path::new("/tmp/a.jsonl")), "the file was found");
        assert!(!table.set_transcript_path(a, Path::new("/tmp/a.jsonl")), "the same file");
        assert_eq!(table.transcript_path(a).as_deref(), Some(Path::new("/tmp/a.jsonl")));

        // The conversation the tail then reads drives the pill.
        let progress = Progress { status: AgentStatus::Working, detail: Some("fix it".to_owned()) };
        let e = table.observe_progress(a, &progress).expect("progress");
        assert_eq!(e.source, AgentSource::Transcript);
        assert_eq!(table.observe_progress(b, &progress), None, "no agent, no progress");

        // The agent exits and the table forgets it.
        table.observe(a, &seen("zsh", None));
        assert!(table.snapshot().is_empty());
    }

    #[test]
    fn a_cleared_conversation_is_looked_up_again_and_the_status_follows_the_new_file() {
        let a = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(a, &seen("claude", None));
        assert!(table.set_transcript_path(a, Path::new("/tmp/first.jsonl")));
        let done = Progress { status: AgentStatus::Done, detail: Some("Fixed.".to_owned()) };
        table.observe_progress(a, &done).expect("the first conversation ends");

        // `/clear` writes a new file: the session keeps asking, and says what it reads now, so
        // the daemon can notice the answer moved and start the tail over.
        assert_eq!(
            table.discoveries().first().and_then(|d| d.current.clone()).as_deref(),
            Some("/tmp/first.jsonl")
        );
        assert!(table.set_transcript_path(a, Path::new("/tmp/second.jsonl")), "the file moved");
        let working = Progress { status: AgentStatus::Working, detail: Some("again".to_owned()) };
        let e = table.observe_progress(a, &working).expect("the new conversation");
        assert_eq!(e.status, AgentStatus::Working, "the pill is not stuck on the old file");

        // A hook names the file itself; that one is never looked up again.
        let b = SessionId::new();
        table.observe(b, &seen("claude", None));
        table.apply(b, &hook(r#"{"hook_event_name":"SessionStart","transcript_path":"/x.jsonl"}"#));
        assert!(table.discoveries().iter().all(|d| d.session != b));
    }

    #[test]
    fn a_session_the_host_no_longer_runs_takes_its_agent_with_it() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(a, &seen("claude", None));
        table.apply(b, &hook(r#"{"hook_event_name":"SessionStart"}"#));
        assert_eq!(table.retain(&[a, b]), Vec::new(), "both still run");
        let gone = table.retain(&[b]);
        assert_eq!(gone.len(), 1, "{gone:#?}");
        assert_eq!((gone[0].session, gone[0].status.clone()), (a, AgentStatus::None));
        assert_eq!(table.snapshot().len(), 1);
        // Even a hooked agent goes when its terminal does; no `SessionEnd` ever arrives.
        assert_eq!(table.retain(&[]).len(), 1);
        assert!(table.snapshot().is_empty());
    }

    #[test]
    fn every_tool_is_described_by_the_argument_that_names_its_work() {
        let detail = |tool: &str, input: &str| {
            hook(&format!(
                r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}","tool_input":{input}}}"#
            ))
            .tool_detail()
        };
        assert_eq!(detail("Bash", r#"{"command":"ls\n-la"}"#).as_deref(), Some("$ ls"));
        assert_eq!(
            detail("Edit", r#"{"file_path":"/Users/x/proj/src/lib.rs"}"#).as_deref(),
            Some("Edit src/lib.rs")
        );
        assert_eq!(detail("Glob", r#"{"pattern":"**/*.rs"}"#).as_deref(), Some("Glob **/*.rs"));
        assert_eq!(detail("Grep", r#"{"pattern":"todo"}"#).as_deref(), Some("Grep todo"));
        assert_eq!(detail("Agent", r#"{"description":"scan"}"#).as_deref(), Some("Agent: scan"));
        assert_eq!(detail("Task", r#"{"description":"scan"}"#).as_deref(), Some("Agent: scan"));
        assert_eq!(
            detail("WebFetch", r#"{"url":"https://x.test"}"#).as_deref(),
            Some("Fetch https://x.test")
        );
        assert_eq!(detail("WebSearch", r#"{"query":"gpui"}"#).as_deref(), Some("Search gpui"));
        assert_eq!(detail("Skill", r#"{"skill":"commit"}"#).as_deref(), Some("/commit"));
        // A tool we do not know, or a known one without its argument, is named bare.
        assert_eq!(detail("Foo", r#"{"x":1}"#).as_deref(), Some("Foo"));
        assert_eq!(detail("Glob", "{}").as_deref(), Some("Glob"));
        // The tool in a permission message stops at a space or a full stop.
        assert_eq!(
            tool_from_message(Some("Claude needs your permission to use Bash.")).as_deref(),
            Some("Bash")
        );
        assert_eq!(
            tool_from_message(Some("Claude needs your permission to use Read tool")).as_deref(),
            Some("Read")
        );
        assert_eq!(tool_from_message(Some("Claude needs your permission to use ")), None);
    }

    #[test]
    fn the_other_notifications_block_on_the_human_and_elicitations_end() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let note = |kind: &str| {
            hook(&format!(r#"{{"hook_event_name":"Notification","notification_type":"{kind}"}}"#))
        };
        let status = |t: &mut Tracker, kind: &str| t.apply(sid, &note(kind)).map(|e| e.status);
        assert_eq!(
            status(&mut t, "agent_needs_input"),
            Some(AgentStatus::Blocked(BlockReason::Question))
        );
        assert_eq!(
            status(&mut t, "elicitation_dialog"),
            Some(AgentStatus::Blocked(BlockReason::Elicitation))
        );
        assert_eq!(status(&mut t, "elicitation_complete"), Some(AgentStatus::Working));
        assert_eq!(
            status(&mut t, "elicitation_url_dialog"),
            Some(AgentStatus::Blocked(BlockReason::Elicitation))
        );
        assert_eq!(status(&mut t, "elicitation_response"), Some(AgentStatus::Working));
    }

    #[test]
    fn the_table_lists_its_agents_and_takes_a_detail_once() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        assert!(table.sessions_with_agents().is_empty());
        table.observe(a, &seen("claude", None));
        table.observe(b, &seen("zsh", None));
        assert_eq!(table.sessions_with_agents(), vec![a]);
        assert!(table.set_detail(a, "fix it"), "a new detail");
        assert!(!table.set_detail(a, "fix it"), "the same detail");
        assert!(!table.set_detail(b, "fix it"), "no agent, nothing to put it on");
        assert_eq!(table.snapshot()[0].detail.as_deref(), Some("fix it"));
    }

    #[test]
    fn an_event_from_another_source_is_not_current_even_with_the_same_status() {
        // The poll said idle from the process; a hook then said idle too. The poll's event
        // would put the weaker source back, so it is stale.
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        let stale = table.observe(sid, &seen("claude", None)).expect("the process");
        assert_eq!((&stale.status, stale.source), (&AgentStatus::Idle, AgentSource::Process));
        let hooked =
            table.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).expect("the hook");
        assert_eq!((&hooked.status, hooked.source), (&AgentStatus::Idle, AgentSource::Hook));
        assert!(!table.is_current(&stale));
        assert!(table.is_current(&hooked));
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
