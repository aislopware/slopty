//! `slopty hook`: the Claude Code hook relay and its installer.
//!
//! Claude Code runs `slopty hook` for every registered event with a JSON payload on stdin. The
//! relay reads `SLOPTY_SESSION` (set by the host for every session it spawns), forwards the
//! payload to `slopty-hostd` over the control socket and exits 0 whatever happens: it must never
//! slow down or block the agent. `slopty hook install` registers it in `~/.claude/settings.json`
//! as an asynchronous exec-form command hook; `uninstall` removes exactly those entries.

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use slopty_agent::HOOK_EVENTS;
use slopty_agent::hooks::{self, Outcome};
use slopty_host::ctl::{CtlReply, CtlRequest};
use slopty_host::manager::SESSION_ENV;

use crate::hostctl;

/// Longest payload the relay forwards; a hook's stdin is a few KB at most.
const PAYLOAD_MAX: u64 = 1 << 20;
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
}

/// Relay stdin to the daemon. Never fails: Claude Code must not notice us.
pub async fn relay() {
    if let Err(e) = relay_inner().await {
        tracing::debug!(error = %e, "hook relay");
    }
}

async fn relay_inner() -> Result<()> {
    let Some(session) = std::env::var_os(SESSION_ENV) else {
        return Ok(());
    };
    let session =
        session.to_string_lossy().parse().context("SLOPTY_SESSION is not a session id")?;
    let mut payload = String::new();
    std::io::stdin().lock().take(PAYLOAD_MAX).read_to_string(&mut payload)?;
    let reply =
        tokio::time::timeout(RELAY_TIMEOUT, hostctl::call(CtlRequest::Hook { session, payload }))
            .await
            .context("daemon did not answer")??;
    if let CtlReply::Error { message } = reply {
        bail!("{message}");
    }
    Ok(())
}

pub fn run(cmd: HookCmd) -> Result<()> {
    match cmd {
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
