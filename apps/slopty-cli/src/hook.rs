//! `slopty hook`: the Claude Code hook relay and its installer.
//!
//! Claude Code runs `slopty hook` for every registered event with a JSON payload on stdin. The
//! relay reads `SLOPTY_SESSION` (set by the worker for every session it spawns), forwards the
//! payload to `slopty-worker` over the control socket and exits 0 whatever happens: it must never
//! slow down or block the agent. `slopty hook install` registers it in `~/.claude/settings.json`
//! as an asynchronous exec-form command hook; `uninstall` removes exactly those entries.
//! `slopty hook report <status> [message]` is the same relay for any program: a wrapper around
//! another agent reports `working|blocked|done|idle|gone` and gets Claude Code's treatment.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Subcommand, ValueEnum};
use slopty_agent::HOOK_EVENTS;
use slopty_agent::hooks::{self, Outcome};
use slopty_worker::ctl::{CtlReply, CtlRequest};
use slopty_worker::manager::SESSION_ENV;

use crate::workerctl;

/// How long the relay waits for the daemon before giving up silently.
const RELAY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Subcommand, Debug)]
pub enum HookCmd {
    /// Register the relay in Claude Code's user settings.
    Install {
        /// Settings file (default: `~/.claude/settings.json`).
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Remove the relay from Claude Code's user settings.
    Uninstall {
        /// Settings file (default: `~/.claude/settings.json`).
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Show whether the relay is registered.
    Status {
        /// Settings file (default: `~/.claude/settings.json`).
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Report this session's agent status yourself (any program, from inside the session).
    Report {
        /// What the agent is doing.
        status: ReportStatus,
        /// One line of detail for the badge.
        message: Vec<String>,
    },
}

/// The words `slopty hook report` takes.
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReportStatus {
    /// Thinking or running something.
    Working,
    /// Needs the human.
    Blocked,
    /// A turn finished.
    Done,
    /// At rest, at its prompt.
    Idle,
    /// No agent here any more.
    Gone,
}

impl ReportStatus {
    const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Gone => "gone",
        }
    }
}

/// The hook payload a report relays: the shape `slopty_agent::Hook` reads as a `Report`.
fn report_payload(status: ReportStatus, message: &[String]) -> String {
    serde_json::json!({
        "hook_event_name": "Report",
        "status": status.word(),
        "message": message.join(" "),
    })
    .to_string()
}

/// Relay stdin to the daemon. Never fails: Claude Code must not notice us.
pub async fn relay(data_dir: &Path) {
    if let Err(e) = relay_stdin(data_dir).await {
        tracing::debug!(error = %e, "hook relay");
    }
}

async fn relay_stdin(data_dir: &Path) -> Result<()> {
    let mut payload = String::new();
    std::io::stdin().lock().read_to_string(&mut payload)?;
    relay_payload(data_dir, forwardable(&payload)?).await
}

/// The part of a hook payload the daemon reads. The payload is read whole, since a
/// `PostToolUse` carries the tool's output and that can run to megabytes, and only the fields
/// [`slopty_agent::Hook`] names go on.
fn forwardable(payload: &str) -> Result<String> {
    let hook = slopty_agent::Hook::parse(payload).context("the hook payload is not JSON")?;
    Ok(serde_json::to_string(&hook)?)
}

/// Relay one payload for the session named by `SLOPTY_SESSION`; nothing to do outside one.
async fn relay_payload(data_dir: &Path, payload: String) -> Result<()> {
    let Some(session) = std::env::var_os(SESSION_ENV) else {
        return Ok(());
    };
    let session =
        session.to_string_lossy().parse().context("SLOPTY_SESSION is not a session id")?;
    let request = workerctl::call(data_dir, CtlRequest::Hook { session, payload });
    let reply =
        tokio::time::timeout(RELAY_TIMEOUT, request).await.context("daemon did not answer")??;
    if let CtlReply::Error { message } = reply {
        bail!("{message}");
    }
    Ok(())
}

pub async fn run(cmd: HookCmd, data_dir: &Path) -> Result<()> {
    match cmd {
        HookCmd::Report { status, message } => {
            if std::env::var_os(SESSION_ENV).is_none() {
                bail!("not inside a Slopty session ({SESSION_ENV} is unset)");
            }
            relay_payload(data_dir, report_payload(status, &message)).await
        }
        HookCmd::Install { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let outcome = hooks::install_at(&path, &relay_command()?)
                .with_context(|| format!("install into {}", path.display()))?;
            println!(
                "{} in {} ({} events)",
                match outcome {
                    Outcome::Changed => "installed",
                    Outcome::Unchanged => "already installed",
                },
                path.display(),
                HOOK_EVENTS.len()
            );
            Ok(())
        }
        HookCmd::Uninstall { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let outcome = hooks::uninstall_at(&path)
                .with_context(|| format!("uninstall from {}", path.display()))?;
            println!(
                "{}",
                match outcome {
                    Outcome::Changed => "removed",
                    Outcome::Unchanged => "not installed",
                }
            );
            Ok(())
        }
        HookCmd::Status { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let registered =
                hooks::registered(&path).with_context(|| format!("read {}", path.display()))?;
            if registered.is_empty() {
                println!("not installed in {}", path.display());
            } else {
                println!("{}/{} events in {}", registered.len(), HOOK_EVENTS.len(), path.display());
            }
            Ok(())
        }
    }
}

fn default_settings() -> PathBuf {
    hooks::settings_path(&hooks::home_dir())
}

/// The exec-form command for this binary.
fn relay_command() -> Result<String> {
    let exe = std::env::current_exe().context("current exe")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(exe.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::{ReportStatus, forwardable, report_payload};

    /// A tool's output of several megabytes is dropped, not cut into invalid JSON: what goes on
    /// reads as the same hook.
    #[test]
    fn a_large_tool_payload_is_forwarded_as_the_fields_the_daemon_reads() {
        let payload = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "t1",
            "tool_input": { "command": "cat big.log" },
            "tool_response": { "stdout": "x".repeat(8 << 20) },
            "transcript_path": "/tmp/t.jsonl",
        })
        .to_string();
        let forwarded = forwardable(&payload).expect("json");
        assert!(forwarded.len() < 1024, "{} bytes", forwarded.len());
        assert_eq!(
            slopty_agent::Hook::parse(&forwarded).expect("json"),
            slopty_agent::Hook::parse(&payload).expect("json")
        );
        forwardable("{\"hook_event_name\": \"Stop\"").unwrap_err();
    }

    #[test]
    fn a_report_is_a_hook_payload_the_tracker_reads() {
        let payload =
            report_payload(ReportStatus::Blocked, &["approve".to_owned(), "it?".to_owned()]);
        let hook = slopty_agent::Hook::parse(&payload).expect("json");
        assert_eq!(hook.event, "Report");
        assert_eq!(hook.status.as_deref(), Some("blocked"));
        assert_eq!(hook.message.as_deref(), Some("approve it?"));
    }
}
