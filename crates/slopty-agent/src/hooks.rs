//! Registering the `slopty hook` relay in Claude Code's user settings.
//!
//! `slopty hook install` writes an entry per event in `~/.claude/settings.json`; the worker
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

/// Is a hook entry ours? Any `slopty` binary with the single argument `hook`: the program in
/// `command` with `args: ["hook"]`, or, without `args`, a shell command line `<program> hook`.
///
/// The program is the whole `command` in the first form, spaces and all: the standard install
/// lives under `~/Library/Application Support`. In the shell form it may be quoted.
#[must_use]
pub fn is_relay(entry: &Value) -> bool {
    let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let program = match entry.get("args").and_then(Value::as_array) {
        Some(args) if args.len() == 1 && args.first() == Some(&json!("hook")) => command,
        Some(_) => return false,
        None => match command.trim_end().strip_suffix(" hook") {
            Some(program) => unquote(program.trim()),
            None => return false,
        },
    };
    Path::new(program).file_name().is_some_and(|n| n == "slopty")
}

/// A shell word without the one pair of quotes around it, if it has them.
fn unquote(word: &str) -> &str {
    ['"', '\'']
        .iter()
        .find_map(|q| word.strip_prefix(*q).and_then(|w| w.strip_suffix(*q)))
        .unwrap_or(word)
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
        // The first relay entry is repointed and any later ones go, with a group they leave
        // empty: settings an older install duplicated collapse to one entry per event.
        let wanted = relay_entry(command);
        let mut found = false;
        groups.retain_mut(|group| {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = entries.len();
            entries.retain_mut(|entry| {
                if !is_relay(entry) {
                    return true;
                }
                if found {
                    return false;
                }
                found = true;
                if *entry != wanted {
                    entry.clone_from(&wanted);
                    changed = true;
                }
                true
            });
            let shrank = entries.len() != before;
            changed |= shrank;
            !(shrank && entries.is_empty())
        });
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
            &json!({"type":"command","command":"/a/b/slopty","args":["worker","status"]})
        ));
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/other hook"})));
        // Both conditions of each form must hold: one argument that is `hook`, or no
        // arguments and a command line that ends in ` hook`.
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/slopty","args":["hook","x"]})));
        assert!(!is_relay(
            &json!({"type":"command","command":"/a/b/slopty hook","args":["worker"]})
        ));
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/slopty"})));
    }

    /// The standard install lives under `~/Library/Application Support`: a space in the path
    /// is still our relay, in either form, so installing twice adds nothing and uninstalling
    /// finds it.
    #[test]
    fn a_relay_under_a_path_with_spaces_is_recognised_installed_once_and_removed() {
        let spaced = "/Users/me/Library/Application Support/Slopty/bin/slopty";
        assert!(is_relay(&json!({"type":"command","command":spaced,"args":["hook"]})));
        for shell in
            [format!("{spaced} hook"), format!("\"{spaced}\" hook"), format!("'{spaced}' hook")]
        {
            assert!(is_relay(&json!({"type":"command","command":shell})), "{shell}");
        }
        assert!(!is_relay(
            &json!({"type":"command","command":"/Users/me/Application Support/other hook"})
        ));

        let home = tempfile::tempdir().expect("tempdir");
        let path = settings_path(home.path());
        assert_eq!(install_at(&path, spaced).expect("install"), Outcome::Changed);
        assert_eq!(install_at(&path, spaced).expect("install"), Outcome::Unchanged);
        let doc = read(&path).expect("read");
        for event in HOOK_EVENTS {
            assert_eq!(doc["hooks"][event].as_array().map(Vec::len), Some(1), "{event}");
        }
        assert_eq!(registered(&path).expect("read").len(), HOOK_EVENTS.len());
        assert_eq!(uninstall_at(&path).expect("uninstall"), Outcome::Changed);
        assert!(registered(&path).expect("read").is_empty());
        assert_eq!(read(&path).expect("read"), json!({}));
    }

    /// Settings an older install filled with one relay group per run collapse to one entry;
    /// a user's hook sharing a group with a duplicate stays.
    #[test]
    fn install_collapses_duplicate_relays() {
        let spaced = "/Users/me/Library/Application Support/Slopty/bin/slopty";
        let ours = json!({"type":"command","command":spaced,"args":["hook"]});
        let mut doc = json!({ "hooks": { "Stop": [
            { "hooks": [ours] },
            { "hooks": [ours] },
            { "matcher": "x", "hooks": [ { "type": "command", "command": "echo hi" }, ours ] },
        ] } });
        assert!(install(&mut doc, spaced));
        assert_eq!(
            doc["hooks"]["Stop"],
            json!([
                { "hooks": [relay_entry(spaced)] },
                { "matcher": "x", "hooks": [ { "type": "command", "command": "echo hi" } ] },
            ])
        );
        assert!(!install(&mut doc, spaced), "collapsed for good");
    }

    #[test]
    fn uninstall_reports_a_change_whether_a_group_shrank_or_went() {
        // Our relay shares a group with a user hook: the entry goes, the group stays.
        let mut doc = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [
                        { "type": "command", "command": "echo hi" },
                        { "type": "command", "command": "/opt/slopty hook" }
                    ] }
                ]
            }
        });
        assert!(uninstall(&mut doc));
        assert_eq!(
            doc,
            json!({ "hooks": { "PreToolUse": [
                { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo hi" } ] }
            ] } })
        );
        // A group that was only ours goes with its event and the `hooks` key.
        let mut doc = json!({ "hooks": { "Stop": [ { "hooks": [
            { "type": "command", "command": "/opt/slopty", "args": ["hook"] }
        ] } ] }, "other": 1 });
        assert!(uninstall(&mut doc));
        assert_eq!(doc, json!({ "other": 1 }));
    }

    #[test]
    fn the_home_is_the_environments_and_a_directory_is_not_a_settings_file() {
        let expected =
            std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        assert_eq!(home_dir(), expected);
        // Only a missing file reads as empty settings; any other error is reported.
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read(&dir.path().join("none.json")).expect("missing"), json!({}));
        assert!(read(dir.path()).is_err(), "a directory is not settings");
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
