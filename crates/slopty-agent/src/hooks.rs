//! Registering the `slopty hook` relay in Claude Code's user settings.
//!
//! `slopty hook install` writes an entry per event in `~/.claude/settings.json`; the worker
//! daemon runs the same code when a client asks (`ClientMsg::InstallHooks`), because the
//! human whose agent Slopty is guessing at may be sitting in front of a phone. The document
//! is edited in place — other people's hooks and every other setting are kept — and written
//! through a sibling temporary file, so a crash never leaves half a settings file behind.
//!
//! Every entry is asynchronous, so the agent never waits on the relay, except
//! `PermissionRequest`: that one runs synchronously so the relay can answer the prompt with a
//! decision from the conversation face ([`crate::permission`]). Until the worker answers, the
//! relay prints nothing at once and Claude Code shows its own dialog, as before. A session's
//! start, a prompt and a turn's end also run `slopty hook reports` synchronously, which hands
//! over the reports waiting for the agent ([`crate::reports`]) and prints nothing otherwise.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::{HOOK_EVENTS, HookEvent, permission, reports, statusline};

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

/// The `slopty` relay shipped beside the running binary: in a bundle every binary lives in
/// `Contents/MacOS`, and in a build tree in `target/<profile>`.
#[must_use]
pub fn relay_beside_this_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("slopty")).filter(|path| path.exists())
}

/// The flag that hands Claude Code MCP servers for one run. Always in its `=` form: the flag
/// takes any number of values, so a value after a space would swallow a prompt that follows.
pub const MCP_CONFIG_FLAG: &str = "--mcp-config";

/// The name Slopty's tools go by in an agent it starts: its tools are `mcp__slopty__…`.
pub const MCP_SERVER_NAME: &str = "slopty";

/// The `--mcp-config` document that serves Slopty's tools through `<command> mcp` on stdio.
///
/// It names no server: `slopty mcp` finds it from the session's
/// [`slopty_proto::project::SERVER_ENV`], which the agent passes on to its MCP servers.
#[must_use]
pub fn mcp_config(command: &str) -> Value {
    json!({
        "mcpServers": {
            MCP_SERVER_NAME: { "type": "stdio", "command": command, "args": ["mcp"] }
        }
    })
}

/// `claude` arguments that also serve Slopty's tools through `<command> mcp`, for that run
/// alone. The caller's own `--mcp-config`s are kept, as Claude Code merges every one it is given.
#[must_use]
pub fn with_mcp(args: Vec<String>, command: &str) -> Vec<String> {
    let flag = format!("{MCP_CONFIG_FLAG}={}", mcp_config(command));
    std::iter::once(flag).chain(args).collect()
}

/// `claude` arguments that also register the relay at `command`, for that run alone, so an
/// agent Slopty starts reports its status whatever this machine's settings say.
///
/// Claude Code keeps only the last `--settings` it is given, whole. So the caller's own (JSON,
/// or a file relative to `cwd`) is read, gets the relay, and becomes the one `--settings`
/// left; one that cannot be read is passed on untouched for Claude Code to report. A handler
/// the user's settings register as well runs once.
///
/// The same `--settings` sets `statusLine` to the status-line wrapper (`<command> hook
/// statusline`), which beats the project's and the user's own status line and then runs it
/// (see [`statusline`] for the precedence). The person's setting keeps its other fields
/// (`padding`, `refreshInterval`); one that came on the caller's `--settings`, which this
/// replaces, travels on the wrapper's command line.
#[must_use]
pub fn with_relay(args: Vec<String>, command: &str, cwd: &Path) -> Vec<String> {
    with_relay_for(args, command, cwd, &statusline::user_settings(&slopty_platform::dirs::home()))
}

/// [`with_relay`] with the user settings file named.
fn with_relay_for(args: Vec<String>, command: &str, cwd: &Path, user: &Path) -> Vec<String> {
    with_settings(args, cwd, |doc| {
        install(doc, command);
        wrap_status_line(doc, command, cwd, user);
    })
}

/// The setting that keeps a session out of the mode that asks no permission at all, whatever
/// its flags or the person's settings say
/// (<https://code.claude.com/docs/en/settings-reference>, `permissions`).
const DISABLE_BYPASS: &str = "disableBypassPermissionsMode";

/// `claude` arguments whose run may not skip its permission prompts.
///
/// They carry `permissions.disableBypassPermissionsMode` set to `"disable"` on the one
/// `--settings`, the caller's own merged in as [`with_relay`] merges them.
///
/// A permission setting takes the strictest value any source gives, and `--settings` ranks
/// above the user's, the project's and the local settings (only managed settings rank above
/// it), so neither `--dangerously-skip-permissions` nor `--permission-mode bypassPermissions`
/// nor a settings file the agent may edit opens the mode again.
#[must_use]
pub fn without_bypass(args: Vec<String>, cwd: &Path) -> Vec<String> {
    with_settings(args, cwd, |doc| {
        let Some(root) = doc.as_object_mut() else { return };
        let permissions = root.entry("permissions").or_insert_with(|| json!({}));
        if !permissions.is_object() {
            *permissions = json!({});
        }
        if let Some(permissions) = permissions.as_object_mut() {
            permissions.insert(DISABLE_BYPASS.to_owned(), json!("disable"));
        }
    })
}

/// Whether a settings document holds the lock [`without_bypass`] puts on.
#[must_use]
pub fn locks_bypass(doc: &Value) -> bool {
    doc.pointer(&format!("/permissions/{DISABLE_BYPASS}")).is_some_and(|v| v == "disable")
}

/// `args` with their one `--settings` (the caller's last one, read as Claude Code reads it,
/// or an empty one) edited by `edit` and put first. One that cannot be read leaves `args` as
/// they are, for Claude Code to report.
fn with_settings(args: Vec<String>, cwd: &Path, edit: impl FnOnce(&mut Value)) -> Vec<String> {
    let mut rest = Vec::with_capacity(args.len());
    let mut given = None;
    let mut words = args.iter().cloned();
    while let Some(word) = words.next() {
        if word == "--" {
            rest.push(word);
            rest.extend(words.by_ref());
        } else if word == SETTINGS_FLAG
            && let Some(value) = words.next()
        {
            given = Some(value);
        } else if let Some(value) = word.strip_prefix("--settings=") {
            given = Some(value.to_owned());
        } else {
            rest.push(word);
        }
    }
    let doc = match given {
        None => Some(json!({})),
        Some(value) => settings_value(&value, cwd),
    };
    let Some(mut doc) = doc else {
        return args;
    };
    edit(&mut doc);
    [SETTINGS_FLAG.to_owned(), doc.to_string()].into_iter().chain(rest).collect()
}

/// Point `statusLine` at the wrapper, keeping the person's own setting's other fields.
fn wrap_status_line(doc: &mut Value, command: &str, cwd: &Path, user: &Path) {
    let given = doc.get("statusLine").filter(|s| statusline::command_of(s).is_some()).cloned();
    let carried = given.as_ref().and_then(statusline::command_of).map(str::to_owned);
    let mut line = given
        .or_else(|| statusline::configured(cwd, user))
        .and_then(|setting| setting.as_object().cloned())
        .unwrap_or_default();
    line.insert("type".to_owned(), json!("command"));
    line.insert(
        "command".to_owned(),
        json!(statusline::wrapper_command(command, carried.as_deref())),
    );
    if let Some(root) = doc.as_object_mut() {
        root.insert("statusLine".to_owned(), Value::Object(line));
    }
}

const SETTINGS_FLAG: &str = "--settings";

/// A `--settings` value as Claude Code reads it: JSON when it opens an object, else a file.
fn settings_value(value: &str, cwd: &Path) -> Option<Value> {
    let text = if value.trim_start().starts_with('{') {
        value.to_owned()
    } else {
        std::fs::read_to_string(cwd.join(value)).ok()?
    };
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
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
pub fn registered(path: &Path) -> std::io::Result<Vec<HookEvent>> {
    let doc = read(path)?;
    Ok(HOOK_EVENTS.into_iter().filter(|event| has_relay(&doc, *event)).collect())
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

/// Replace the settings file whole (`slopty_platform::fs::replace`), so a crash never leaves
/// half of one, and a settings file that is a link into a dotfiles repository stays one.
///
/// # Errors
///
/// When the directory cannot be created or the file cannot be replaced.
pub fn write(path: &Path, doc: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    text.push('\n');
    slopty_platform::fs::replace(path, text.as_bytes())
}

/// Is a hook entry ours? Any `slopty` binary as `command` with `args: ["hook"]`.
///
/// That is the form [`install`] writes. The program is the whole `command`, spaces and all:
/// the standard install lives under `~/Library/Application Support`.
#[must_use]
pub fn is_relay(entry: &Value) -> bool {
    let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let args = entry.get("args").and_then(Value::as_array).map(Vec::as_slice);
    matches!(args, Some([word]) if word == "hook")
        && Path::new(command).file_name().is_some_and(|n| n == "slopty")
}

/// Is a hook entry the reports hook? Any `slopty` binary with `args: ["hook", "reports"]`.
#[must_use]
pub fn is_reports(entry: &Value) -> bool {
    let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let args = entry.get("args").and_then(Value::as_array).map(Vec::as_slice);
    matches!(args, Some([hook, reports]) if hook == "hook" && reports == "reports")
        && Path::new(command).file_name().is_some_and(|n| n == "slopty")
}

/// The reports hook's entry: synchronous, since what it prints is what Claude Code reads.
fn reports_entry(command: &str) -> Value {
    json!({
        "type": "command",
        "command": command,
        "args": ["hook", "reports"],
        "timeout": HOOK_TIMEOUT_S,
    })
}

/// The relay's entry for `event`: asynchronous, except for a permission request, which waits
/// for a decision.
fn relay_entry(command: &str, event: HookEvent) -> Value {
    if event == HookEvent::PermissionRequest {
        return json!({
            "type": "command",
            "command": command,
            "args": ["hook"],
            "timeout": permission::HOOK_TIMEOUT_S,
        });
    }
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
pub fn has_relay(doc: &Value, event: HookEvent) -> bool {
    doc.get("hooks").and_then(|h| h.get(event.as_str())).and_then(Value::as_array).is_some_and(
        |groups| {
            groups.iter().any(|g| {
                g.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(is_relay))
            })
        },
    )
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
        let groups = hooks.entry(event.as_str()).or_insert_with(|| Value::Array(Vec::new()));
        if !groups.is_array() {
            *groups = Value::Array(Vec::new());
            changed = true;
        }
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        changed |= ensure(groups, relay_entry(command, event), is_relay);
        if reports::EVENTS.contains(&event) {
            changed |= ensure(groups, reports_entry(command), is_reports);
        }
    }
    changed
}

/// Put `wanted` in place of the entry `ours` finds among `groups`, or in a group of its own.
/// Whether that changed anything.
fn ensure(groups: &mut Vec<Value>, wanted: Value, ours: fn(&Value) -> bool) -> bool {
    let found = groups
        .iter_mut()
        .filter_map(|group| group.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten()
        .find(|entry| ours(entry));
    match found {
        Some(entry) if *entry == wanted => false,
        Some(entry) => {
            *entry = wanted;
            true
        }
        None => {
            groups.push(json!({ "hooks": [wanted] }));
            true
        }
    }
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
                entries.retain(|e| !is_relay(e) && !is_reports(e));
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

    /// Slopty's tools reach an agent it starts through `slopty mcp`, in the flag's `=` form so
    /// a prompt after it stays a prompt, and the caller's own MCP servers stay beside them.
    #[test]
    fn a_started_agent_is_handed_slopty_mcp() {
        let args = with_mcp(
            vec!["--mcp-config".to_owned(), "mine.json".to_owned(), "fix it".to_owned()],
            "/opt/slopty",
        );
        let (ours, rest) = args.split_first().expect("the flag");
        assert_eq!(rest, ["--mcp-config", "mine.json", "fix it"]);
        let doc: Value =
            serde_json::from_str(ours.strip_prefix("--mcp-config=").expect(ours)).expect("json");
        let server = &doc["mcpServers"]["slopty"];
        assert_eq!(server["command"], "/opt/slopty");
        assert_eq!(server["args"], json!(["mcp"]));
        assert_eq!(server["type"], "stdio");
        assert!(server.get("env").is_none(), "the session's own environment names the server");
    }

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
        assert!(
            doc["hooks"].get("MessageDisplay").is_none(),
            "held TUI paint: measured before registered"
        );
        // A moved binary is repointed, not duplicated.
        assert!(install(&mut doc, "/usr/local/bin/slopty"));
        let pre = doc["hooks"]["PreToolUse"].as_array().expect("array");
        assert_eq!(pre.len(), 2, "existing user hook kept alongside ours");
        assert!(uninstall(&mut doc));
        assert_eq!(doc, original);
        assert!(!uninstall(&mut doc));
    }

    /// The relay rides along on `--settings`: added when the caller passes none, merged into
    /// the caller's JSON or file (the last one given, as Claude Code reads them), and never
    /// taken from after `--`. An unreadable value is left for Claude Code to report.
    #[test]
    fn a_run_gets_the_relay_on_the_one_settings_it_keeps() {
        let relay = "/opt/Slopty/slopty";
        let dir = tempfile::tempdir().expect("tempdir");
        let user = dir.path().join("user-settings.json");
        let with_relay =
            |args: Vec<String>, relay: &str, cwd: &Path| with_relay_for(args, relay, cwd, &user);
        let words = |args: &[&str]| args.iter().map(|&a| a.to_owned()).collect::<Vec<_>>();
        let settings = |out: &[String]| -> Value {
            assert_eq!(
                out.iter()
                    .take_while(|a| *a != "--")
                    .filter(|a| a.starts_with(SETTINGS_FLAG))
                    .count(),
                1,
                "{out:?}"
            );
            assert_eq!(out.first().map(String::as_str), Some(SETTINGS_FLAG));
            serde_json::from_str(out.get(1).expect("value")).expect("json")
        };
        let registers = |doc: &Value| HOOK_EVENTS.into_iter().all(|event| has_relay(doc, event));

        let out = with_relay(words(&["--model", "opus"]), relay, dir.path());
        let doc = settings(&out);
        assert!(registers(&doc));
        assert_eq!(doc["hooks"]["Stop"][0]["hooks"][0], relay_entry(relay, HookEvent::Stop));
        assert_eq!(out.get(2..), Some(words(&["--model", "opus"]).as_slice()));

        let args = ["--settings", r#"{"model":"haiku"}"#, "-p", "--settings={\"theme\":\"dark\"}"];
        let doc = settings(&with_relay(words(&args), relay, dir.path()));
        assert!(registers(&doc));
        assert_eq!((doc.get("model"), &doc["theme"]), (None, &json!("dark")), "the last one wins");

        std::fs::write(dir.path().join("ci.json"), r#"{"env":{"CI":"1"}}"#).expect("write");
        let doc = settings(&with_relay(words(&["--settings", "ci.json"]), relay, dir.path()));
        assert!(registers(&doc));
        assert_eq!(doc["env"]["CI"], json!("1"), "a file is read from the working directory");

        let out = with_relay(words(&["--", "--settings", "x"]), relay, dir.path());
        assert!(registers(&settings(&out)));
        assert_eq!(out.get(2..), Some(words(&["--", "--settings", "x"]).as_slice()));

        for unreadable in
            [&["--settings", "missing.json"][..], &["--settings", "{nope"], &["--settings=[1]"]]
        {
            assert_eq!(with_relay(words(unreadable), relay, dir.path()), words(unreadable));
        }
    }

    /// A run that may not skip its prompts carries the lock on its one `--settings`, beside
    /// the relay and the caller's own permissions; nothing else is added.
    #[test]
    fn a_run_without_permission_flags_locks_bypass_mode_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = dir.path().join("user-settings.json");
        let settings = |out: &[String]| -> Value {
            assert_eq!(out.iter().filter(|a| a.starts_with(SETTINGS_FLAG)).count(), 1, "{out:?}");
            serde_json::from_str(out.get(1).expect("value")).expect("json")
        };
        let mine = r#"{"permissions":{"allow":["Bash(git *)"],"defaultMode":"plan"}}"#;
        let args = ["--settings", mine, "--dangerously-skip-permissions"].map(str::to_owned);
        let relayed = with_relay_for(args.to_vec(), "/opt/slopty", dir.path(), &user);
        let locked = without_bypass(relayed.clone(), dir.path());
        let doc = settings(&locked);
        assert_eq!(
            doc["permissions"],
            json!({ "allow": ["Bash(git *)"], "defaultMode": "plan", DISABLE_BYPASS: "disable" })
        );
        assert!(has_relay(&doc, HookEvent::SessionStart), "the relay stays");
        assert_eq!(locked.get(2..), Some(&["--dangerously-skip-permissions".to_owned()][..]));
        assert!(
            settings(&relayed).pointer("/permissions/disableBypassPermissionsMode").is_none(),
            "only a locked run carries it"
        );
        let bare = without_bypass(Vec::new(), dir.path());
        assert_eq!(settings(&bare), json!({ "permissions": { DISABLE_BYPASS: "disable" } }));
        let odd = without_bypass(
            ["--settings", r#"{"permissions":true}"#].map(str::to_owned).to_vec(),
            dir.path(),
        );
        assert_eq!(settings(&odd)["permissions"], json!({ DISABLE_BYPASS: "disable" }));
    }

    /// The run's status line is the wrapper, in front of the person's own: theirs from the
    /// caller's `--settings` travels on the wrapper's command line, theirs from the files is
    /// looked up when it runs, and either keeps its other fields.
    #[test]
    fn a_run_gets_the_status_line_wrapper_in_front_of_the_persons_own() {
        let relay = "/opt/Slopty/slopty";
        let dir = tempfile::tempdir().expect("tempdir");
        let user = dir.path().join("user-settings.json");
        let line = |args: &[&str]| -> Value {
            let args = args.iter().map(|&a| a.to_owned()).collect();
            let out = with_relay_for(args, relay, dir.path(), &user);
            let doc: Value = serde_json::from_str(out.get(1).expect("value")).expect("json");
            doc["statusLine"].clone()
        };
        assert_eq!(
            line(&[]),
            json!({ "type": "command", "command": "/opt/Slopty/slopty hook statusline" })
        );
        let given = r#"{"statusLine":{"type":"command","command":"my-line --short","padding":1}}"#;
        assert_eq!(
            line(&["--settings", given]),
            json!({
                "type": "command",
                "command": "/opt/Slopty/slopty hook statusline --command 'my-line --short'",
                "padding": 1,
            })
        );
        std::fs::write(
            &user,
            r#"{"statusLine":{"type":"command","command":"u.sh","refreshInterval":5}}"#,
        )
        .expect("write");
        assert_eq!(
            line(&[]),
            json!({
                "type": "command",
                "command": "/opt/Slopty/slopty hook statusline",
                "refreshInterval": 5,
            }),
            "found in the files: the wrapper looks it up itself"
        );
        let ours = json!({ "statusLine": { "command": statusline::wrapper_command(relay, None) } });
        assert_eq!(
            line(&["--settings", &ours.to_string()])["command"],
            json!("/opt/Slopty/slopty hook statusline"),
            "the wrapper never wraps itself"
        );
    }

    /// Ours is a `slopty` program with the one argument `hook`, the form [`install`] writes;
    /// a shell command line is somebody else's.
    #[test]
    fn recognises_our_command() {
        assert!(is_relay(&json!({"type":"command","command":"/a/b/slopty","args":["hook"]})));
        assert!(!is_relay(
            &json!({"type":"command","command":"/a/b/slopty","args":["worker","status"]})
        ));
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/other","args":["hook"]})));
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/slopty","args":["hook","x"]})));
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/slopty hook"})));
        assert!(!is_relay(&json!({"type":"command","command":"/a/b/slopty"})));
    }

    /// The standard install lives under `~/Library/Application Support`: a space in the path
    /// is still our relay, so installing twice adds nothing and uninstalling finds it.
    #[test]
    fn a_relay_under_a_path_with_spaces_is_recognised_installed_once_and_removed() {
        let spaced = "/Users/me/Library/Application Support/Slopty/bin/slopty";
        assert!(is_relay(&json!({"type":"command","command":spaced,"args":["hook"]})));

        let home = tempfile::tempdir().expect("tempdir");
        let path = settings_path(home.path());
        assert_eq!(install_at(&path, spaced).expect("install"), Outcome::Changed);
        assert_eq!(install_at(&path, spaced).expect("install"), Outcome::Unchanged);
        let doc = read(&path).expect("read");
        for event in HOOK_EVENTS {
            let groups = if reports::EVENTS.contains(&event) { 2 } else { 1 };
            let got = doc["hooks"][event.as_str()].as_array().map(Vec::len);
            assert_eq!(
                got,
                Some(groups),
                "{event}: the relay, and where reports go the reports hook"
            );
        }
        let stop = &doc["hooks"]["Stop"][1]["hooks"][0];
        assert!(is_reports(stop) && stop.get("async").is_none(), "synchronous: {stop}");
        assert_eq!(registered(&path).expect("read").len(), HOOK_EVENTS.len());
        assert_eq!(uninstall_at(&path).expect("uninstall"), Outcome::Changed);
        assert!(registered(&path).expect("read").is_empty());
        assert_eq!(read(&path).expect("read"), json!({}));
    }

    #[test]
    fn uninstall_reports_a_change_whether_a_group_shrank_or_went() {
        // Our relay shares a group with a user hook: the entry goes, the group stays.
        let mut doc = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [
                        { "type": "command", "command": "echo hi" },
                        { "type": "command", "command": "/opt/slopty", "args": ["hook"] }
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
    fn a_directory_is_not_a_settings_file() {
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
