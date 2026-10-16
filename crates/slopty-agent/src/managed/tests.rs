use std::path::Path;

use serde_json::json;

use super::*;

fn settings(doc: &Value) -> ManagedSettings {
    let mut settings = ManagedSettings::default();
    settings.add(doc);
    settings
}

/// No managed settings leave everything Slopty adds in place.
#[test]
fn nothing_managed_forbids_nothing() {
    let none = ManagedSettings::default();
    assert!(none.sideloads());
    assert!(none.admits_mcp("slopty", &["/bin/slopty", "mcp"]));
}

/// Sideloading off takes Slopty's tools, and so does confining MCP to plugins; confining
/// another surface does not.
#[test]
fn each_key_reads_as_what_it_costs() {
    let sideload = settings(&json!({ "disableSideloadFlags": true }));
    assert!(!sideload.sideloads() && !sideload.admits_mcp("slopty", &[]));
    let plugin_hooks = settings(&json!({ "strictPluginOnlyCustomization": ["hooks"] }));
    assert!(plugin_hooks.admits_mcp("slopty", &[]), "only hooks are confined");
    let plugin_all = settings(&json!({ "strictPluginOnlyCustomization": true }));
    assert!(!plugin_all.admits_mcp("slopty", &[]));
}

/// The MCP lists: a deny wins, an allow list admits only what it names, and the managed-only
/// switch with no list admits nothing.
#[test]
fn the_mcp_lists_admit_slopty_by_name_or_command() {
    let command = ["/opt/slopty", "mcp"];
    let denied = settings(&json!({ "deniedMcpServers": [{ "serverName": "slopty" }] }));
    assert!(!denied.admits_mcp("slopty", &command));
    let by_name = settings(&json!({ "allowedMcpServers": [{ "serverName": "slopty" }] }));
    assert!(by_name.admits_mcp("slopty", &command));
    let by_command =
        settings(&json!({ "allowedMcpServers": [{ "serverCommand": ["/opt/slopty", "mcp"] }] }));
    assert!(by_command.admits_mcp("slopty", &command));
    let others = settings(&json!({ "allowedMcpServers": [{ "serverUrl": "https://x.example" }] }));
    assert!(!others.admits_mcp("slopty", &command));
    let lockdown = settings(&json!({ "allowedMcpServers": [] }));
    assert!(!lockdown.admits_mcp("slopty", &command));
    let managed_only = settings(&json!({ "allowManagedMcpServersOnly": true }));
    assert!(!managed_only.admits_mcp("slopty", &command));
}

/// Only the listed keys are kept: an `env` block's other names and values, the rest of the
/// document, never reach the reader.
#[test]
fn the_reader_keeps_only_the_listed_keys() {
    let doc = json!({
        "env": {
            "ANTHROPIC_API_KEY": "sk-secret",
            "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1",
        },
        "apiKeyHelper": "/usr/local/bin/secret",
        "availableModels": ["opus", "sonnet"],
        "enforceAvailableModels": true,
        "permissions": { "defaultMode": "auto", "deny": ["Bash(rm:*)"] },
    });
    let read = settings(&doc);
    let shown = format!("{read:?}");
    assert!(!shown.contains("secret") && !shown.contains("rm:"), "{shown}");
    assert!(read.enforce_available_models);
    assert_eq!(
        read.available_models.as_deref(),
        Some(&["opus".to_owned(), "sonnet".to_owned()][..])
    );
    assert_eq!(read.default_mode.as_deref(), Some("auto"));
}

/// Both files are read, a switch either turns on is on, and a file that is missing or broken
/// says nothing.
#[test]
fn the_files_are_read_together() {
    let dir = tempfile::tempdir().unwrap();
    let system = dir.path().join("managed-settings.json");
    let remote = dir.path().join("remote-settings.json");
    let broken = dir.path().join("broken.json");
    std::fs::write(
        &system,
        r#"{"disableSideloadFlags": false, "deniedMcpServers": [{"serverName": "a"}]}"#,
    )
    .unwrap();
    std::fs::write(
        &remote,
        r#"{"disableSideloadFlags": true, "deniedMcpServers": [{"serverName": "b"}]}"#,
    )
    .unwrap();
    std::fs::write(&broken, "{").unwrap();
    let missing = dir.path().join("missing.json");
    let read = ManagedSettings::from_files(&[system, remote, broken, missing]);
    assert!(read.disable_sideload_flags);
    assert_eq!(read.denied_mcp_servers.len(), 2);
}

/// A managed launcher is told by its answer to `--managed-help`; Claude Code refuses the flag.
#[test]
fn the_launcher_is_told_by_its_managed_help() {
    let usage = "Usage: claude managed <COMMAND>\n  login  Sign in to the managed fleet\n";
    assert_eq!(Launcher::from_managed_help(true, usage), Launcher::Managed);
    assert_eq!(
        Launcher::from_managed_help(false, "error: unknown option '--managed-help'"),
        Launcher::Plain
    );
    assert_eq!(Launcher::from_managed_help(true, "2.1.289 (Claude Code)"), Launcher::Plain);
    assert!(Launcher::Managed.is_managed() && !Launcher::Plain.is_managed());
}

/// A managed client's version is in its path; any other path names none.
#[test]
fn a_managed_clients_version_is_in_its_path() {
    let client =
        Path::new("/Users/me/.local/share/claude-managed/artifacts/2.1.289/darwin-arm64/claude");
    assert_eq!(artifact_version(client).as_deref(), Some("2.1.289"));
    assert!(is_artifact(client));
    let native = Path::new("/Users/me/.local/share/claude/versions/2.1.289");
    assert_eq!(artifact_version(native), None);
    let short = Path::new("/x/claude-managed/artifacts/2.1.289");
    assert_eq!(artifact_version(short), None, "no client under it");
}

/// The newest artifact is the newest version by number, not by name.
#[test]
fn the_newest_artifact_is_the_newest_by_number() {
    let data = tempfile::tempdir().unwrap();
    let artifacts = data.path().join("claude-managed").join("artifacts");
    for v in ["2.1.9", "2.1.289", "2.1.30", "notes"] {
        std::fs::create_dir_all(artifacts.join(v)).unwrap();
    }
    assert_eq!(newest_artifact(data.path()).as_deref(), Some("2.1.289"));
    assert_eq!(newest_artifact(&data.path().join("none")), None);
}
