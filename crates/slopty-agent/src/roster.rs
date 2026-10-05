//! Claude Code's own registry of its live sessions: `~/.claude/sessions/<pid>.json`.
//!
//! Hooks are the one signal with a memory: a permission prompt or a question was said once, by
//! a hook, and a worker that restarts meanwhile has lost it (the process, the title and the
//! transcript it reads again cannot say "blocked"). Each live Claude Code keeps a file there
//! with its pid, its conversation and whether it is busy, waiting on the person (and on what) or
//! idle; `claude agents` lists those files (<https://code.claude.com/docs/en/agent-view>). The
//! worker reads them itself once after it starts ([`crate::AgentTable::recover`]): running
//! `claude agents` would cost a process, and through a managed launcher a full launch
//! ([`crate::managed`]). Only the `<pid>.json` files are read, never their `.key` siblings.
//!
//! The fields are read leniently: every one is optional, unknown ones are ignored, and a
//! status or wait this build does not know says nothing. A file whose process is gone is
//! passed over.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::status::{AgentStatus, BlockReason};

/// One session as its registry file says it.
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
    /// `interactive`, or how it runs in the background (`bg`, `daemon`, `daemon-worker`).
    #[serde(default)]
    pub kind: Option<String>,
    /// The Claude Code release it runs.
    #[serde(default)]
    pub version: Option<String>,
    /// `busy`, `waiting`, `idle` or `shell`.
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

/// The registry's directory, as Claude Code finds it: under `CLAUDE_CONFIG_DIR` when set,
/// else `~/.claude`.
#[must_use]
pub fn sessions_dir(home: &Path) -> PathBuf {
    std::env::var_os(crate::trust::CONFIG_DIR_ENV)
        .map_or_else(|| home.join(".claude"), PathBuf::from)
        .join("sessions")
}

/// The sessions registered in `dir` ([`sessions_dir`]) whose process is `alive`.
///
/// Only a file named by its pid as Claude Code writes it (`<pid>.json`) is read, and the name
/// gives the pid. A file that cannot be read or parsed is passed over, as is the directory.
#[must_use]
pub fn registered(dir: &Path, alive: impl Fn(i32) -> bool) -> Vec<Listed> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut listed: Vec<Listed> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let pid = registry_pid(name.to_str()?)?;
            if !alive(pid) {
                return None;
            }
            let text = std::fs::read_to_string(entry.path()).ok()?;
            let session: Listed = serde_json::from_str(&text).ok()?;
            Some(Listed { pid: Some(pid), ..session })
        })
        .collect();
    listed.sort_by_key(|l| l.pid);
    listed
}

/// The pid a registry file's name gives: `<pid>.json` in canonical decimal, nothing else.
fn registry_pid(name: &str) -> Option<i32> {
    let digits = name.strip_suffix(".json")?;
    let pid: i32 = digits.parse().ok()?;
    (pid > 1 && pid.to_string() == digits).then_some(pid)
}

/// The conversations `listed` runs in the background (`claude --bg`) now: `claude --resume`
/// refuses one of them while it runs, and `claude attach <id>` opens it.
pub fn background(listed: &[Listed]) -> impl Iterator<Item = &str> {
    listed
        .iter()
        .filter(|l| l.kind.as_deref().is_some_and(|kind| kind != "interactive"))
        .filter(|l| l.pid.is_some())
        .filter_map(|l| l.session_id.as_deref())
}

/// The live session in `listed` that holds conversation `session`, if one does: taking it up
/// again elsewhere would make a second writer.
#[must_use]
pub fn holder<'a>(listed: &'a [Listed], session: &str) -> Option<&'a Listed> {
    listed.iter().find(|l| l.pid.is_some() && l.session_id.as_deref() == Some(session))
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

    fn registry(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    /// Every live session's file is read, its pid from its name; a dead one, a key, a file of
    /// another name or shape, and a broken one say nothing.
    #[test]
    fn the_registry_lists_the_live_sessions() {
        let dir = registry(&[
            (
                "101.json",
                r#"{"pid":101,"sessionId":"s-1","cwd":"/w","kind":"interactive","status":"waiting",
                   "waitingFor":"permission prompt","version":"2.1.289","peerProtocol":1}"#,
            ),
            ("102.json", r#"{"sessionId":"s-2","kind":"bg","status":"idle"}"#),
            ("103.json", r#"{"sessionId":"s-dead","status":"busy"}"#),
            ("101.key", "never read"),
            ("0101.json", r#"{"sessionId":"s-padded"}"#),
            ("notes.json", r#"{"sessionId":"s-notes"}"#),
            ("104.json", "{"),
        ]);
        let listed = registered(dir.path(), |pid| pid != 103);
        let pids: Vec<_> = listed.iter().map(|l| (l.pid, l.session_id.as_deref())).collect();
        assert_eq!(pids, [(Some(101), Some("s-1")), (Some(102), Some("s-2"))]);
        assert_eq!(listed[0].version.as_deref(), Some("2.1.289"));
        assert_eq!(
            listed[0].status(),
            Some(AgentStatus::Blocked(BlockReason::Permission { tool: String::new() }))
        );
        assert_eq!(registered(&dir.path().join("none"), |_| true), Vec::new());
    }

    /// Only a live background session is one to attach to: an interactive one, one whose
    /// process is gone, and one that names no conversation are not.
    #[test]
    fn background_sessions_are_the_live_ones_run_with_bg() {
        let dir = registry(&[
            ("10.json", r#"{"sessionId":"bg-1","kind":"bg","status":"idle"}"#),
            ("11.json", r#"{"sessionId":"bg-gone","kind":"bg"}"#),
            ("12.json", r#"{"sessionId":"tty-1","kind":"interactive"}"#),
            ("13.json", r#"{"kind":"bg"}"#),
            ("14.json", r#"{"sessionId":"unsaid"}"#),
        ]);
        let listed = registered(dir.path(), |pid| pid != 11);
        assert_eq!(background(&listed).collect::<Vec<_>>(), ["bg-1"]);
    }
}
