//! `claude agents --json`: Claude Code's own list of its live sessions.
//!
//! Hooks are the one signal with a memory: a permission prompt or a question was said once, by
//! a hook, and a worker that restarts meanwhile has lost it (the process, the title and the
//! transcript it reads again cannot say "blocked"). Claude Code lists every live session on the
//! machine, interactive ones included, with its pid, its conversation and whether it is busy,
//! waiting on the person (and on what) or idle
//! (<https://code.claude.com/docs/en/agent-view>, "the supported way to read session state
//! from outside Claude Code"). The worker reads the list once after it starts
//! ([`crate::AgentTable::recover`]) and takes what it says for the agents it finds by pid.
//!
//! The fields are read leniently: every one is optional, unknown ones are ignored, and a
//! status or wait this build does not know says nothing.

use serde::Deserialize;
use slopty_proto::agent::{AgentStatus, BlockReason};

/// The command's arguments after `claude`.
pub const ARGS: [&str; 2] = ["agents", "--json"];

/// One session as `claude agents --json` lists it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    /// The session's process, while it is alive.
    #[serde(default)]
    pub pid: Option<i32>,
    /// The conversation (Claude Code's `session_id`).
    #[serde(default)]
    pub session_id: Option<String>,
    /// Where it was started.
    #[serde(default)]
    pub cwd: Option<String>,
    /// `interactive` or `background`.
    #[serde(default)]
    pub kind: Option<String>,
    /// While the process is alive: `busy`, `waiting` or `idle`.
    #[serde(default)]
    pub status: Option<String>,
    /// While `waiting`: `permission prompt`, `input needed`, `sandbox request`,
    /// `worker request` or `dialog open`.
    #[serde(default)]
    pub waiting_for: Option<String>,
}

impl Listed {
    /// The status it says, as a hook would have put it; `None` for one this build does not
    /// know. A permission (or sandbox) prompt names no tool: the list does not.
    #[must_use]
    pub fn status(&self) -> Option<AgentStatus> {
        Some(match self.status.as_deref()? {
            "busy" => AgentStatus::Working,
            "idle" => AgentStatus::Idle,
            "waiting" => AgentStatus::Blocked(match self.waiting_for.as_deref() {
                Some("permission prompt" | "sandbox request") => {
                    BlockReason::Permission { tool: String::new() }
                }
                _ => BlockReason::Question,
            }),
            _ => return None,
        })
    }
}

/// The sessions in the command's output.
///
/// # Errors
///
/// When the output is not a JSON array of objects.
pub fn parse(json: &str) -> Result<Vec<Listed>, serde_json::Error> {
    serde_json::from_str(json)
}

/// The conversations `listed` runs in the background (`claude --bg`) now: `claude --resume`
/// refuses one of them while it runs, and `claude attach <id>` opens it.
pub fn background(listed: &[Listed]) -> impl Iterator<Item = &str> {
    listed
        .iter()
        .filter(|l| l.kind.as_deref() == Some("background") && l.pid.is_some())
        .filter_map(|l| l.session_id.as_deref())
}

/// The subcommand that opens a background session in this terminal (`claude attach <id>`).
pub const ATTACH: &str = "attach";

#[cfg(test)]
mod tests {
    use super::*;

    /// Each status and wait reads as the pill a hook gives it; the unknown says nothing.
    #[test]
    fn statuses_read_as_the_hooks_would_say_them() {
        let listed = |status: &str, waiting: Option<&str>| Listed {
            status: Some(status.to_owned()),
            waiting_for: waiting.map(str::to_owned),
            ..Listed::default()
        };
        assert_eq!(listed("busy", None).status(), Some(AgentStatus::Working));
        assert_eq!(listed("idle", None).status(), Some(AgentStatus::Idle));
        assert_eq!(
            listed("waiting", Some("permission prompt")).status(),
            Some(AgentStatus::Blocked(BlockReason::Permission { tool: String::new() }))
        );
        assert_eq!(
            listed("waiting", Some("input needed")).status(),
            Some(AgentStatus::Blocked(BlockReason::Question))
        );
        assert_eq!(listed("dreaming", None).status(), None);
        assert_eq!(Listed::default().status(), None, "a background session whose process ended");
    }

    /// Only a live background session is one to attach to: an interactive one, one whose
    /// process is gone, and one that names no conversation are not.
    #[test]
    fn background_sessions_are_the_live_ones_run_with_bg() {
        let listed = parse(
            r#"[
                {"pid": 10, "sessionId": "bg-1", "kind": "background", "status": "idle"},
                {"sessionId": "bg-gone", "kind": "background"},
                {"pid": 11, "sessionId": "tty-1", "kind": "interactive"},
                {"pid": 12, "kind": "background"}
            ]"#,
        )
        .unwrap();
        assert_eq!(background(&listed).collect::<Vec<_>>(), ["bg-1"]);
    }
}
