//! What the worker does beside a driven Claude Code, from the person's own settings.
//!
//! These are the permission rules it writes into a scope's file, the `!` command it runs in the
//! chat's folder, and the status line command it runs as Claude Code's TUI would.
//!
//! - **Settings, by scope.** The person's (`~/.claude/settings.json`), the project's
//!   (`.claude/settings.json`) and the project's on this machine (`.claude/settings.local.json`),
//!   read with Claude Code's precedence: local over project over the person's. Only the setting
//!   asked for is read; a rule changes only the scope's `permissions` list, every other member of
//!   the file kept. Credentials are never read or written: no file but these three is opened.
//! - **`!` commands** run in the person's shell (`$SHELL -c`, else `/bin/sh`) in the chat's folder,
//!   for at most [`SHELL_LIMIT`], each stream kept to its last [`SHELL_KEPT`] characters as Claude
//!   Code keeps a command's output; while one runs it is a call of its own in the thread, its
//!   output streaming.
//! - **The status line** runs the person's `statusLine.command` with the documented JSON on its
//!   standard input, at most once per [`STATUS_EVERY`], for [`STATUS_LIMIT`] at most; its first
//!   lines, colour kept, are the footer's text.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use slopty_agent::claude::driven::Ran;
use slopty_core::WallMs;
use slopty_proto::thread::detail::{ExecDetail, ExecStatus};
use slopty_proto::thread::{
    Action, Clipped, IntentId, Item, ItemBody, ItemId, PartKey, Rule, RuleChange, ThreadId,
    ThreadState, ToolCall, ToolDetail, ToolState, kind,
};
use tokio::io::AsyncReadExt as _;

use crate::thread::Host;

/// The longest a `!` command runs before it is ended.
pub const SHELL_LIMIT: Duration = Duration::from_mins(10);

/// The most of each of a `!` command's streams kept, from its end (Claude Code's own
/// `bashOutputMaxChars` default).
pub const SHELL_KEPT: usize = 30_000;

/// How often the status line runs at most.
pub const STATUS_EVERY: Duration = Duration::from_millis(300);

/// The longest the status line command may take.
pub const STATUS_LIMIT: Duration = Duration::from_secs(5);

/// The settings file of `scope` ([`Rule::USER`], [`Rule::PROJECT`], [`Rule::LOCAL`]) for a
/// session in `cwd`, the person's home being `home` (Claude Code's own directory under it).
#[must_use]
pub fn file_of(home: &Path, cwd: &Path, scope: &str) -> Option<PathBuf> {
    match scope {
        Rule::USER => Some(slopty_agent::claude::sessions::config_dir(home).join("settings.json")),
        Rule::PROJECT => Some(cwd.join(".claude").join("settings.json")),
        Rule::LOCAL => Some(cwd.join(".claude").join("settings.local.json")),
        _ => None,
    }
}

/// Setting `key` as the session in `cwd` has it: the most local scope that sets it.
#[must_use]
pub fn setting(home: &Path, cwd: &Path, key: &str) -> Option<Value> {
    [Rule::LOCAL, Rule::PROJECT, Rule::USER].iter().find_map(|scope| {
        let text = std::fs::read_to_string(file_of(home, cwd, scope)?).ok()?;
        let doc: Value = serde_json::from_str(&text).ok()?;
        doc.get(key).filter(|v| !v.is_null()).cloned()
    })
}

/// What is wrong with `change`, in words, if anything.
///
/// # Errors
///
/// A behaviour, a scope or a rule Claude Code does not take.
pub fn checked(change: &RuleChange) -> Result<(), String> {
    if ![Rule::ALLOW, Rule::ASK, Rule::DENY].contains(&change.behavior.as_str()) {
        return Err(format!("There is no permission behaviour {}", change.behavior));
    }
    if ![Rule::USER, Rule::PROJECT, Rule::LOCAL].contains(&change.scope.as_str()) {
        return Err(format!("Rules are not written to {}", change.scope));
    }
    if change.rule.trim().is_empty() || change.rule.contains('\n') {
        return Err("A rule is one line, and not empty".to_owned());
    }
    Ok(())
}

/// Write `change` into its scope's file for a session in `cwd`: the rule added to (or taken
/// from) its behaviour's list, every other member of the file as it was.
///
/// # Errors
///
/// The change is not one Claude Code takes ([`checked`]), or the file cannot be read as JSON
/// or written.
pub fn change_rule(home: &Path, cwd: &Path, change: &RuleChange) -> Result<(), String> {
    checked(change)?;
    let path = file_of(home, cwd, &change.scope).ok_or("no such scope")?;
    let mut doc: Value = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not JSON: {e}", path.display()))?,
        Ok(_) => json!({}),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    let fields = doc.as_object_mut().ok_or_else(|| format!("{} is no object", path.display()))?;
    let permissions = fields.entry("permissions").or_insert_with(|| json!({}));
    let permissions = permissions
        .as_object_mut()
        .ok_or_else(|| format!("{}'s permissions are no object", path.display()))?;
    let list = permissions.entry(change.behavior.clone()).or_insert_with(|| json!([]));
    let list = list
        .as_array_mut()
        .ok_or_else(|| format!("{}'s {} rules are no list", path.display(), change.behavior))?;
    let rule = Value::String(change.rule.clone());
    if change.add {
        if !list.contains(&rule) {
            list.push(rule);
        }
    } else {
        list.retain(|r| *r != rule);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    let staged = path.with_extension("json.slopty");
    std::fs::write(&staged, format!("{text}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::rename(&staged, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The last `kept` characters of `text`.
fn tail(text: &str, kept: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(kept)).collect()
}

/// The call a `!` command is in the thread while it runs.
fn chip(id: &ItemId, command: &str, turn: slopty_proto::thread::TurnId, now: WallMs) -> Item {
    let call = ToolCall {
        name: "Bash".to_owned(),
        kind: kind::EXEC.to_owned(),
        title: format!("!{command}"),
        input: Clipped::whole(command),
        state: ToolState::Running,
        output: None,
        images: Vec::new(),
        detail: Some(ToolDetail::Exec(ExecDetail {
            command: Clipped::whole(command),
            description: None,
            cwd: None,
            background: false,
            task: None,
            status: ExecStatus::Running,
            exit_code: None,
            stderr: None,
            duration_ms: None,
            edits: None,
            interpretation: None,
        })),
        child: None,
        ended_ms: None,
    };
    Item { id: id.clone(), turn, at_ms: now, body: ItemBody::Tool(Box::new(call)) }
}

/// The id of the call `!` command `intent` is while it runs.
#[must_use]
pub fn chip_id(intent: IntentId) -> ItemId {
    ItemId(format!("bang-{intent}"))
}

/// Run `!` command `command` for `thread` in `cwd`, intent `intent`: a call of its own in the
/// thread while it runs, its output streaming, gone once it ended. What it did.
pub async fn run_shell(
    host: &Host,
    thread: ThreadId,
    cwd: &Path,
    command: &str,
    intent: IntentId,
) -> Ran {
    let id = chip_id(intent);
    let turn = host
        .state(thread)
        .and_then(|(s, _)| s.last_turn().map(|t| t.id))
        .unwrap_or(slopty_proto::thread::TurnId::BEFORE);
    host.apply(thread, vec![Action::ItemStarted(chip(&id, command, turn, WallMs::now()))]);
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty());
    let shell = shell.unwrap_or_else(|| "/bin/sh".to_owned());
    let child = tokio::process::Command::new(&shell)
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut ran = Ran { command: command.to_owned(), ..Ran::default() };
    match child {
        Ok(mut child) => {
            let (out, err) = (child.stdout.take(), child.stderr.take());
            let streamed = async {
                let stdout = async {
                    let mut said = String::new();
                    let Some(mut out) = out else { return said };
                    let mut buf = vec![0_u8; 8192];
                    while let Ok(n) = out.read(&mut buf).await {
                        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else { break };
                        let text = String::from_utf8_lossy(chunk).into_owned();
                        let part = PartKey::Output;
                        host.apply(
                            thread,
                            vec![Action::Append { item: id.clone(), part, text: text.clone() }],
                        );
                        said.push_str(&text);
                    }
                    said
                };
                let stderr = async {
                    let mut said = Vec::new();
                    if let Some(mut err) = err {
                        let _read = err.read_to_end(&mut said).await;
                    }
                    String::from_utf8_lossy(&said).into_owned()
                };
                let (stdout, stderr) = tokio::join!(stdout, stderr);
                let _status = child.wait().await;
                (stdout, stderr)
            };
            match tokio::time::timeout(SHELL_LIMIT, streamed).await {
                Ok((stdout, stderr)) => {
                    ran.stdout = tail(stdout.trim_end(), SHELL_KEPT);
                    ran.stderr = tail(stderr.trim_end(), SHELL_KEPT);
                }
                Err(_) => {
                    ran.stderr = format!("Ended after {} minutes", SHELL_LIMIT.as_secs() / 60);
                }
            }
        }
        Err(e) => ran.stderr = format!("{shell}: {e}"),
    }
    host.apply(thread, vec![Action::ItemRemoved { item: id }]);
    ran
}

/// What the status line command reads on its standard input, from `state`: the session, its
/// model, its folder, its cost and its context, as Claude Code documents it.
#[must_use]
pub fn status_input(state: &ThreadState, transcript: &Path) -> Value {
    let meters = &state.meters;
    let model_id = meters.model_id.clone().unwrap_or_default();
    let used = meters.context_tokens.unwrap_or_default();
    let window = meters.context_window.unwrap_or_default();
    let share = used
        .saturating_mul(100)
        .checked_div(window)
        .map_or(Value::Null, |p| Value::from(p.min(100)));
    json!({
        "hook_event_name": "Status",
        "session_id": state.meta.native,
        "transcript_path": transcript,
        "cwd": state.meta.cwd,
        "model": { "id": model_id, "display_name": meters.model.clone().unwrap_or_default() },
        "workspace": { "current_dir": state.meta.cwd, "project_dir": state.meta.cwd },
        "version": state.meta.agent_version,
        "output_style": { "name": "default" },
        "context_window": {
            "total_input_tokens": used,
            "context_window_size": window,
            "used_percentage": share,
        },
    })
}

/// Run the status line command `command` with `input` on its standard input in `cwd`: its
/// first lines, colour kept, or `None` when it fails or says nothing.
pub async fn status_line(command: &str, input: &Value, cwd: &Path) -> Option<String> {
    use tokio::io::AsyncWriteExt as _;
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input = input.to_string();
    let ran = async {
        let _wrote = stdin.write_all(input.as_bytes()).await;
        drop(stdin);
        child.wait_with_output().await.ok()
    };
    let output = tokio::time::timeout(STATUS_LIMIT, ran).await.ok().flatten()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim_end_matches(['\n', '\r']);
    (output.status.success() && !text.trim().is_empty()).then(|| text.to_owned())
}

/// The status line command the person set for sessions in `cwd`, if any.
#[must_use]
pub fn status_command(home: &Path, cwd: &Path) -> Option<String> {
    let line = setting(home, cwd, "statusLine")?;
    if line.get("type").and_then(Value::as_str).is_some_and(|t| t != "command") {
        return None;
    }
    line.get("command").and_then(Value::as_str).filter(|c| !c.trim().is_empty()).map(str::to_owned)
}

/// Whether Claude Code answers a `!` command's output for sessions in `cwd`
/// (`respondToBashCommands`, on unless it is set off).
#[must_use]
pub fn answers_bang(home: &Path, cwd: &Path) -> bool {
    setting(home, cwd, "respondToBashCommands").and_then(|v| v.as_bool()) != Some(false)
}

#[cfg(test)]
mod tests;
