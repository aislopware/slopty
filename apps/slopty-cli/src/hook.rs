//! `slopty hook`: the Claude Code hook relay and its installer.
//!
//! Claude Code runs `slopty hook` for every registered event with a JSON payload on stdin. The
//! relay reads `SLOPTY_SESSION` (set by the host for every session it spawns), forwards the
//! payload to `slopty-hostd` over the control socket and exits 0 whatever happens: it must never
//! slow down or block the agent. `slopty hook install` registers it in `~/.claude/settings.json`
//! as an asynchronous exec-form command hook; `uninstall` removes exactly those entries.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use serde_json::{Map, Value, json};
use slopty_agent::HOOK_EVENTS;
use slopty_host::ctl::{CtlReply, CtlRequest};
use slopty_host::manager::SESSION_ENV;

use crate::hostctl;

/// Longest payload the relay forwards; a hook's stdin is a few KB at most.
const PAYLOAD_MAX: u64 = 1 << 20;
/// How long the relay waits for the daemon before giving up silently.
const RELAY_TIMEOUT: Duration = Duration::from_secs(2);
/// Hook `timeout` written to settings (seconds).
const HOOK_TIMEOUT_S: u32 = 5;

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
            let mut doc = read_settings(&path)?;
            let changed = install(&mut doc, &relay_command()?);
            if changed {
                write_settings(&path, &doc)?;
            }
            println!(
                "{} in {} ({} events)",
                if changed { "installed" } else { "already installed" },
                path.display(),
                HOOK_EVENTS.len()
            );
            Ok(())
        }
        HookCmd::Uninstall { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let mut doc = read_settings(&path)?;
            let changed = uninstall(&mut doc);
            if changed {
                write_settings(&path, &doc)?;
            }
            println!("{}", if changed { "removed" } else { "not installed" });
            Ok(())
        }
        HookCmd::Status { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let doc = read_settings(&path)?;
            let registered: Vec<&str> =
                HOOK_EVENTS.iter().copied().filter(|ev| has_relay(&doc, ev)).collect();
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
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join(".claude").join("settings.json")
}

fn read_settings(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => {
            let doc: Value =
                serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
            if !doc.is_object() {
                bail!("{} is not a JSON object", path.display());
            }
            Ok(doc)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Write via a sibling temp file so a crash never leaves a half-written settings file.
fn write_settings(path: &Path, doc: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.slopty-tmp");
    let mut text = serde_json::to_string_pretty(doc)?;
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}

/// The exec-form command for this binary.
fn relay_command() -> Result<String> {
    let exe = std::env::current_exe().context("current exe")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(exe.to_string_lossy().into_owned())
}

/// Is a hook entry ours? Any `slopty` binary with the single argument `hook`.
fn is_relay(entry: &Value) -> bool {
    let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let args = entry.get("args").and_then(Value::as_array);
    let by_args = args.is_some_and(|a| a.len() == 1 && a.first() == Some(&json!("hook")));
    let shell_form = args.is_none() && command.ends_with(" hook");
    Path::new(command.split(' ').next().unwrap_or("")).file_name().is_some_and(|n| n == "slopty")
        && (by_args || shell_form)
}

fn relay_entry(command: &str) -> Value {
    json!({
        "type": "command",
        "command": command,
        "args": ["hook"],
        "async": true,
        "timeout": HOOK_TIMEOUT_S,
    })
}

fn has_relay(doc: &Value, event: &str) -> bool {
    doc.get("hooks").and_then(|h| h.get(event)).and_then(Value::as_array).is_some_and(|groups| {
        groups.iter().any(|g| {
            g.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(is_relay))
        })
    })
}

/// Add (or repoint) the relay for every event. Returns whether the document changed.
fn install(doc: &mut Value, command: &str) -> bool {
    let mut changed = false;
    let Some(root) = doc.as_object_mut() else {
        return false;
    };
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
        changed = true;
    }
    let Some(hooks) = hooks.as_object_mut() else {
        return changed;
    };
    for event in HOOK_EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| Value::Array(Vec::new()));
        if !groups.is_array() {
            *groups = Value::Array(Vec::new());
            changed = true;
        }
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        let wanted = relay_entry(command);
        let mut found = false;
        let entries = groups
            .iter_mut()
            .filter_map(|g| g.get_mut("hooks"))
            .filter_map(Value::as_array_mut)
            .flatten();
        for entry in entries {
            if is_relay(entry) {
                found = true;
                if *entry != wanted {
                    entry.clone_from(&wanted);
                    changed = true;
                }
            }
        }
        if !found {
            groups.push(json!({ "hooks": [wanted] }));
            changed = true;
        }
    }
    changed
}

/// Remove the relay everywhere, pruning empty groups, events and the `hooks` key.
fn uninstall(doc: &mut Value) -> bool {
    let mut changed = false;
    let Some(hooks) = doc.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut empty_events = Vec::new();
    for (event, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else { continue };
        for group in groups.iter_mut() {
            if let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = entries.len();
                entries.retain(|e| !is_relay(e));
                changed |= entries.len() != before;
            }
        }
        let before = groups.len();
        groups.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|hs| !hs.is_empty()));
        changed |= groups.len() != before;
        if groups.is_empty() {
            empty_events.push(event.clone());
        }
    }
    for event in empty_events {
        hooks.remove(&event);
    }
    if hooks.is_empty()
        && let Some(root) = doc.as_object_mut()
    {
        root.remove("hooks");
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_uninstall_restores() {
        let mut doc = json!({
            "permissions": { "allow": ["Bash(git *)"] },
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo hi" } ] }
                ]
            }
        });
        let original = doc.clone();
        assert!(install(&mut doc, "/opt/slopty"));
        assert!(!install(&mut doc, "/opt/slopty"));
        for ev in HOOK_EVENTS {
            assert!(has_relay(&doc, ev), "{ev} registered");
        }
        assert!(!has_relay(&doc, "PreCompact"));
        // A moved binary is repointed, not duplicated.
        assert!(install(&mut doc, "/usr/local/bin/slopty"));
        let pre = doc["hooks"]["PreToolUse"].as_array().expect("array");
        assert_eq!(pre.len(), 2, "existing user hook kept alongside ours");
        assert!(uninstall(&mut doc));
        assert_eq!(doc, original);
        assert!(!uninstall(&mut doc));
    }

    #[test]
    fn recognises_both_forms_of_our_command() {
        assert!(is_relay(&json!({"type":"command","command":"/a/b/slopty","args":["hook"]})));
        assert!(is_relay(&json!({"type":"command","command":"/a/b/slopty hook"})));
        assert!(!is_relay(
            &json!({"type":"command","command":"/a/b/slopty","args":["host","status"]})
        ));
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/other hook"})));
    }

    #[test]
    fn empty_settings_become_hooks_only() {
        let mut doc = json!({});
        assert!(install(&mut doc, "/opt/slopty"));
        assert_eq!(doc["hooks"]["Stop"][0]["hooks"][0]["args"], json!(["hook"]));
        assert!(uninstall(&mut doc));
        assert_eq!(doc, json!({}));
    }
}
