//! Claude Code's status line, as Slopty reads it.
//!
//! Claude Code runs a status-line command after each assistant message (debounced 300 ms) with
//! JSON on stdin: the model, how full the context window is, the session's cost, the rate
//! limits. An agent Slopty starts gets a wrapper in that place (`slopty hook statusline`, put on
//! `--settings` by [`crate::hooks::with_relay`]). The wrapper forwards the [`Meters`] to the
//! worker as a [`HookEvent::Statusline`] hook, the same way the relay posts hooks, then runs the
//! person's own status-line command and passes its output through unchanged, so their line
//! looks as it did.
//!
//! **Precedence.** Claude Code takes `statusLine` from the first of these that sets it: managed
//! settings, `--settings`, the project's `.claude/settings.local.json`, its
//! `.claude/settings.json`, and the user's `settings.json` (under `CLAUDE_CONFIG_DIR`, else
//! `~/.claude`). The wrapper rides on `--settings`, so it wins over every file except managed
//! settings. The person's own line is then the one the caller's `--settings` named, which
//! `with_relay` replaces and so carries on the wrapper's command line, or else the first of the
//! three files that names one, looked up when the wrapper runs ([`configured`]) so an edit to
//! them shows at once. A managed `statusLine` beats the wrapper: the organisation decided, and
//! the face goes without meters.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{Hook, HookEvent};

/// The wrapper's words after the `slopty` binary.
const WRAPPER_WORDS: &str = "hook statusline";

pub use slopty_proto::conversation::{Meters, RateWindow};

/// The meters in Claude Code's status-line input; what is missing stays `None`.
#[must_use]
pub fn meters(status: &Value) -> Meters {
    let at = |path: &str| status.pointer(path);
    let text = |path: &str| at(path).and_then(Value::as_str).map(str::to_owned);
    let window = |name: &str| {
        let window = at(&format!("/rate_limits/{name}"))?;
        Some(RateWindow {
            used_pct: window.get("used_percentage")?.as_f64()?,
            resets_at: window.get("resets_at").and_then(Value::as_u64),
        })
    };
    Meters {
        model: text("/model/display_name"),
        model_id: text("/model/id"),
        context_used_pct: at("/context_window/used_percentage").and_then(Value::as_f64),
        context_window: at("/context_window/context_window_size").and_then(Value::as_u64),
        cost_usd: at("/cost/total_cost_usd").and_then(Value::as_f64),
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
    }
}

/// The hook the wrapper forwards for one status-line input.
#[must_use]
pub fn hook(status: &Value) -> Hook {
    let text = |path: &str| status.pointer(path).and_then(Value::as_str).map(str::to_owned);
    Hook {
        event: HookEvent::Statusline,
        session_id: text("/session_id"),
        transcript_path: text("/transcript_path"),
        cwd: text("/workspace/current_dir").or_else(|| text("/cwd")),
        meters: Some(meters(status)),
        ..Hook::default()
    }
}

/// The wrapper's shell command (`statusLine` commands run in a shell): the `slopty` at
/// `slopty`, and the person's own command when it has to travel with it.
#[must_use]
pub fn wrapper_command(slopty: &str, theirs: Option<&str>) -> String {
    let mut command = format!("{} {WRAPPER_WORDS}", shell_quote(slopty));
    if let Some(theirs) = theirs {
        command.push_str(" --command ");
        command.push_str(&shell_quote(theirs));
    }
    command
}

/// Whether a status-line command is the wrapper (never run it from itself).
fn is_wrapper(command: &str) -> bool {
    command.contains(&format!(" {WRAPPER_WORDS}"))
}

/// A word the shell reads back as `word`.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The command of a `statusLine` setting, unless it is the wrapper's.
#[must_use]
pub fn command_of(setting: &Value) -> Option<&str> {
    setting
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty() && !is_wrapper(c))
}

/// The user's settings file, as Claude Code finds it: under `CLAUDE_CONFIG_DIR` when set, else
/// `~/.claude`.
#[must_use]
pub fn user_settings(home: &Path) -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map_or_else(|| home.join(".claude"), PathBuf::from)
        .join("settings.json")
}

/// The person's own `statusLine` setting for an agent in `project`.
///
/// It is the first of the project's local and shared settings and `user` (the user settings
/// file) that names a command other than the wrapper. Files that are missing or unreadable are
/// passed over.
#[must_use]
pub fn configured(project: &Path, user: &Path) -> Option<Value> {
    let files = [
        project.join(".claude").join("settings.local.json"),
        project.join(".claude").join("settings.json"),
        user.to_path_buf(),
    ];
    files.iter().find_map(|file| {
        let text = std::fs::read_to_string(file).ok()?;
        let setting = serde_json::from_str::<Value>(&text).ok()?.get("statusLine")?.clone();
        command_of(&setting)?;
        Some(setting)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The documented input, read into meters; a sparse one reads as what it has.
    #[test]
    fn the_meters_are_read_from_the_status_line_input() {
        let status = json!({
            "session_id": "s1", "transcript_path": "/t.jsonl", "cwd": "/w",
            "model": { "id": "claude-opus-5-5", "display_name": "Opus" },
            "workspace": { "current_dir": "/w/sub", "project_dir": "/w" },
            "cost": { "total_cost_usd": 0.012_34, "total_duration_ms": 45_000 },
            "context_window": { "context_window_size": 200_000, "used_percentage": 8 },
            "rate_limits": {
                "five_hour": { "used_percentage": 23.5, "resets_at": 1_738_425_600 },
                "seven_day": { "used_percentage": 41.2 }
            }
        });
        let hook = hook(&status);
        assert_eq!(hook.event, HookEvent::Statusline);
        assert_eq!(
            (hook.session_id.as_deref(), hook.transcript_path.as_deref(), hook.cwd.as_deref()),
            (Some("s1"), Some("/t.jsonl"), Some("/w/sub"))
        );
        assert_eq!(
            hook.meters,
            Some(Meters {
                model: Some("Opus".into()),
                model_id: Some("claude-opus-5-5".into()),
                context_used_pct: Some(8.0),
                context_window: Some(200_000),
                cost_usd: Some(0.012_34),
                five_hour: Some(RateWindow { used_pct: 23.5, resets_at: Some(1_738_425_600) }),
                seven_day: Some(RateWindow { used_pct: 41.2, resets_at: None }),
            })
        );
        let early = json!({ "context_window": { "used_percentage": null }, "model": {} });
        assert_eq!(meters(&early), Meters::default());
    }

    /// The wrapper's command survives the shell, its own is recognised, and the person's
    /// setting is found in the order Claude Code reads the files, the wrapper skipped.
    #[test]
    fn the_persons_status_line_is_found_where_claude_code_looks() {
        let command = wrapper_command("/Apps/Slop ty/slopty", Some("echo 'hi' | cat"));
        assert_eq!(
            command,
            r"'/Apps/Slop ty/slopty' hook statusline --command 'echo '\''hi'\'' | cat'"
        );
        assert!(is_wrapper(&command));
        assert!(!is_wrapper("~/.claude/statusline.sh"));

        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("p");
        let user = dir.path().join("home").join("settings.json");
        std::fs::create_dir_all(project.join(".claude")).expect("mkdir");
        std::fs::create_dir_all(user.parent().expect("dir")).expect("mkdir");
        assert_eq!(configured(&project, &user), None, "no files");
        std::fs::write(
            &user,
            r#"{"statusLine":{"type":"command","command":"user.sh","padding":2}}"#,
        )
        .expect("write");
        assert_eq!(configured(&project, &user).and_then(|s| s["padding"].as_u64()), Some(2));
        std::fs::write(project.join(".claude/settings.json"), "{not json").expect("write");
        let wrapper = json!({"statusLine": {"type": "command", "command": command}});
        std::fs::write(project.join(".claude/settings.local.json"), wrapper.to_string())
            .expect("write");
        let found = configured(&project, &user).expect("found");
        assert_eq!(
            command_of(&found),
            Some("user.sh"),
            "the wrapper and a broken file are passed over"
        );
        std::fs::write(
            project.join(".claude/settings.json"),
            r#"{"statusLine":{"command":"project.sh"}}"#,
        )
        .expect("write");
        assert_eq!(configured(&project, &user).as_ref().and_then(command_of), Some("project.sh"));
    }
}
