//! Registering the `slopty hook` relay in Claude Code's user settings.
//!
//! `slopty hook install` writes an entry per event in `~/.claude/settings.json`; the host
//! daemon runs the same code when a client asks (`ClientMsg::InstallHooks`), because the
//! human whose agent Slopty is guessing at may be sitting in front of a phone. The document
//! is edited in place — other people's hooks and every other setting are kept — and written
//! through a sibling temporary file, so a crash never leaves half a settings file behind.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::HOOK_EVENTS;

/// Hook `timeout` written to settings (seconds).
const HOOK_TIMEOUT_S: u32 = 5;

/// What editing the settings did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The document changed and was written.
    Changed,
    /// It already said what we wanted.
    Unchanged,
}

/// Claude Code's user settings file under `home`.
#[must_use]
pub fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// `$HOME`, or `/tmp` when the environment does not say.
#[must_use]
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

/// Register the relay at `command` for every event in the settings at `path`.
///
/// # Errors
///
/// When the file exists and is not a JSON object, or cannot be read or replaced.
pub fn install_at(path: &Path, command: &str) -> std::io::Result<Outcome> {
    let mut doc = read(path)?;
    if !install(&mut doc, command) {
        return Ok(Outcome::Unchanged);
    }
    write(path, &doc)?;
    Ok(Outcome::Changed)
}

/// Remove the relay from the settings at `path`.
///
/// # Errors
///
/// As [`install_at`].
pub fn uninstall_at(path: &Path) -> std::io::Result<Outcome> {
    let mut doc = read(path)?;
    if !uninstall(&mut doc) {
        return Ok(Outcome::Unchanged);
    }
    write(path, &doc)?;
    Ok(Outcome::Changed)
}

/// The events of [`HOOK_EVENTS`] the settings at `path` register the relay for.
///
/// # Errors
///
/// As [`install_at`].
pub fn registered(path: &Path) -> std::io::Result<Vec<&'static str>> {
    let doc = read(path)?;
    Ok(HOOK_EVENTS.iter().copied().filter(|event| has_relay(&doc, event)).collect())
}

/// Read the settings document; a missing or empty file is an empty object.
///
/// # Errors
///
/// When the file cannot be read or is not a JSON object.
pub fn read(path: &Path) -> std::io::Result<Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let doc: Value = serde_json::from_str(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("parse {}: {e}", path.display()),
        )
    })?;
    if !doc.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a JSON object", path.display()),
        ));
    }
    Ok(doc)
}

/// Write via a sibling temp file so a crash never leaves a half-written settings file.
///
/// # Errors
///
/// When the directory cannot be created or the file cannot be replaced.
pub fn write(path: &Path, doc: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.slopty-tmp");
    let mut text = serde_json::to_string_pretty(doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Is a hook entry ours? Any `slopty` binary with the single argument `hook`.
#[must_use]
pub fn is_relay(entry: &Value) -> bool {
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

/// Whether `doc` registers the relay for `event`.
#[must_use]
pub fn has_relay(doc: &Value, event: &str) -> bool {
    doc.get("hooks").and_then(|h| h.get(event)).and_then(Value::as_array).is_some_and(|groups| {
        groups.iter().any(|g| {
            g.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(is_relay))
        })
    })
}

/// Add (or repoint) the relay for every event. Returns whether the document changed.
pub fn install(doc: &mut Value, command: &str) -> bool {
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
pub fn uninstall(doc: &mut Value) -> bool {
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

    #[test]
    fn installing_into_a_home_creates_and_then_leaves_the_file_alone() {
        let home = tempfile::tempdir().expect("tempdir");
        let path = settings_path(home.path());
        assert!(registered(&path).expect("read").is_empty(), "no file yet");
        assert_eq!(install_at(&path, "/opt/slopty").expect("install"), Outcome::Changed);
        assert_eq!(registered(&path).expect("read").len(), HOOK_EVENTS.len());
        assert_eq!(install_at(&path, "/opt/slopty").expect("install"), Outcome::Unchanged);
        assert_eq!(uninstall_at(&path).expect("uninstall"), Outcome::Changed);
        assert!(registered(&path).expect("read").is_empty());
        assert_eq!(uninstall_at(&path).expect("uninstall"), Outcome::Unchanged);
    }

    #[test]
    fn a_settings_file_that_is_not_an_object_is_an_error() {
        let home = tempfile::tempdir().expect("tempdir");
        let path = settings_path(home.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "[1, 2]").expect("write");
        read(&path).unwrap_err();
        std::fs::write(&path, "   ").expect("write");
        assert_eq!(read(&path).expect("empty is an object"), json!({}));
    }
}
