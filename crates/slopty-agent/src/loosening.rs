//! What loosens a running Claude Code's permissions, read off its own command line
//! (`docs/decisions/projects.md`, "An agent never has more than the person gave it").
//!
//! The server refuses flags that loosen permissions when it starts `claude`, but it sees only
//! the arguments it was handed: `sh -c "claude --allowedTools Bash"` in a terminal an agent
//! opened passes that check, and a loose mode shows in the hooks while pre-approved tools do
//! not. So the worker reads the command line of the agent actually in the foreground of each
//! terminal, whatever started it, and says what in it loosens permissions; the server judges
//! whether the terminal may run so.
//!
//! A `--settings` document loosens when it allows tools itself (`permissions.allow`), starts
//! in a looser mode (`permissions.defaultMode`), or adds a `PermissionRequest` or `PreToolUse`
//! hook other than Slopty's own, since such a hook may approve what the person would be asked.
//! One the worker cannot read counts as loosening: it is judged by what it could hold.

use std::path::Path;

use serde_json::Value;
use slopty_proto::project::{LOOSENING_FLAGS, PERMISSION_MODE_FLAG, SAFE_MODES};

use crate::detect;
use crate::hooks::{is_relay, is_reports};

/// The events whose hooks may answer a permission for the person.
const DECIDING: [&str; 2] = ["PermissionRequest", "PreToolUse"];

/// What in the command line `argv` of a Claude Code process (running in `cwd`) loosens its
/// permissions, as a reader would name it; empty when nothing does.
#[must_use]
pub fn loosening(argv: &[String], cwd: &Path) -> Vec<String> {
    let args = detect::agent_args(argv);
    let words: Vec<String> = if args.is_empty() { shell_words(argv) } else { args.to_vec() };
    let mut found = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        let (flag, inline) =
            word.split_once('=').map_or((word.as_str(), None), |(f, v)| (f, Some(v)));
        if flag == PERMISSION_MODE_FLAG {
            let mode = inline.map(str::to_owned).or_else(|| words.next().cloned());
            if !mode.as_deref().is_some_and(|m| SAFE_MODES.contains(&m)) {
                found.push(format!("{PERMISSION_MODE_FLAG} {}", mode.unwrap_or_default()));
            }
        } else if flag == "--settings" {
            let value = inline.map(str::to_owned).or_else(|| words.next().cloned());
            found.extend(settings(value.as_deref().unwrap_or_default(), cwd));
        } else if LOOSENING_FLAGS.contains(&flag) {
            found.push(flag.to_owned());
        }
    }
    found
}

/// The words of the command a shell runs (`sh -c '<command>'`), split as a shell would split
/// plain words; the whole line when it names none. Quoting is dropped, not interpreted: a flag
/// is a flag wherever it stands.
fn shell_words(argv: &[String]) -> Vec<String> {
    let mut rest = argv.iter().skip(1);
    let command = loop {
        match rest.next() {
            Some(flag) if flag.starts_with('-') && flag.contains('c') => break rest.next(),
            Some(_) => {}
            None => break None,
        }
    };
    let Some(command) = command else { return argv.to_vec() };
    command
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '\'' || c == '"').to_owned())
        .collect()
}

/// What in the `--settings` value `value` (JSON, or a file under `cwd`) loosens permissions.
fn settings(value: &str, cwd: &Path) -> Vec<String> {
    let text = if value.trim_start().starts_with('{') {
        Some(value.to_owned())
    } else {
        std::fs::read_to_string(cwd.join(value)).ok()
    };
    let Some(doc) = text.and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return vec![format!("--settings {value}, which the worker cannot read")];
    };
    let mut found = Vec::new();
    if doc.pointer("/permissions/allow").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
        found.push("--settings allowing tools (permissions.allow)".to_owned());
    }
    if let Some(mode) = doc.pointer("/permissions/defaultMode").and_then(Value::as_str)
        && !SAFE_MODES.contains(&mode)
    {
        found.push(format!("--settings starting in {mode} (permissions.defaultMode)"));
    }
    for event in DECIDING {
        let groups = doc.pointer(&format!("/hooks/{event}")).and_then(Value::as_array);
        let entries = groups.into_iter().flatten().filter_map(|g| g.get("hooks")?.as_array());
        if entries.flatten().any(|entry| !is_relay(entry) && !is_reports(entry)) {
            found.push(format!("--settings with a {event} hook of its own"));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    /// Flags that loosen are found however `claude` was started: bare, under a runtime, or in
    /// a shell's command; safe modes and other flags are not.
    #[test]
    fn loose_flags_are_found_however_claude_was_started() {
        let cwd = Path::new("/");
        assert_eq!(loosening(&words("claude --allowedTools Bash"), cwd), ["--allowedTools"]);
        assert_eq!(
            loosening(&words("node /opt/claude/cli.js --dangerously-skip-permissions"), cwd),
            ["--dangerously-skip-permissions"]
        );
        let wrapped =
            vec!["sh".to_owned(), "-c".to_owned(), "claude '--allowed-tools=Edit' -c".to_owned()];
        assert_eq!(loosening(&wrapped, cwd), ["--allowed-tools"]);
        assert_eq!(
            loosening(&words("claude --permission-mode=bypassPermissions"), cwd),
            ["--permission-mode bypassPermissions"]
        );
        assert!(loosening(&words("claude --permission-mode plan --model opus"), cwd).is_empty());
        assert!(loosening(&words("claude -- --allowedTools"), cwd).is_empty(), "a prompt's words");
    }

    /// Slopty's own `--settings` (its hooks, bypass locked off) loosens nothing; one that allows
    /// tools, starts loose, or adds a deciding hook of its own does, and so does one the worker
    /// cannot read, from a file or not.
    #[test]
    fn a_settings_document_loosens_by_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let ours = json!({
            "hooks": { "PermissionRequest": [{ "hooks": [
                { "type": "command", "command": "/x/slopty", "args": ["hook"] },
            ]}]},
            "permissions": { "disableBypassPermissionsMode": "disable" },
        });
        let argv =
            |doc: &Value| vec!["claude".to_owned(), "--settings".to_owned(), doc.to_string()];
        assert!(loosening(&argv(&ours), dir.path()).is_empty());
        let allowing = json!({ "permissions": { "allow": ["Bash(rm:*)"] } });
        assert_eq!(loosening(&argv(&allowing), dir.path()).len(), 1);
        let loose = json!({ "permissions": { "defaultMode": "acceptEdits" } });
        assert!(loosening(&argv(&loose), dir.path())[0].contains("acceptEdits"));
        let hooked = json!({ "hooks": { "PreToolUse": [{ "hooks": [
            { "type": "command", "command": "approve-everything" },
        ]}]}});
        assert!(loosening(&argv(&hooked), dir.path())[0].contains("PreToolUse"));
        std::fs::write(dir.path().join("s.json"), allowing.to_string()).unwrap();
        let from_file = words("claude --settings=s.json");
        assert_eq!(loosening(&from_file, dir.path()).len(), 1, "read from the agent's directory");
        let missing = words("claude --settings missing.json");
        assert!(loosening(&missing, dir.path())[0].contains("cannot read"));
    }
}
