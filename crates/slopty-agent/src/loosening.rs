//! What loosens a Claude Code's permissions, read off its command line
//! (`docs/decisions/projects.md`, "An agent never has more than the person gave it").
//!
//! One judgment serves two places. The server reads the arguments it is handed to start
//! `claude` ([`args`]), and refuses a start they loosen. The worker reads the command line of
//! the agent actually in the foreground of each terminal, however it was started ([`loosening`]):
//! bare, under a runtime, or inside a shell's line (`sh -c "cd x && claude --allowedTools
//! Bash"`), a wrapper the server never saw; the server judges whether that terminal may run so.
//!
//! A flag loosens unless Claude Code documents it as giving nothing the person would be asked
//! for ([`SAFE_FLAGS`]): a new flag is judged before it is let through. So does a `--` or any
//! word that starts with a dash and is not a known flag, since a flag's value that looks like a
//! flag, or options after `--`, are parsed by rules this does not repeat.
//!
//! Four flags are judged by their values, against what the worker adds itself ([`Own`]):
//! - `--permission-mode`, by the mode: one that asks no less than `default` ([`SAFE_MODES`]);
//! - `--settings`, a document (JSON, or a file under the agent's directory) that holds only what is
//!   known to loosen nothing: the worker's own hooks and status line, deny and ask rules, bypass
//!   locked off, a mode that asks, and settings of display or model (`SAFE_SETTINGS`). Anything
//!   else, an allow rule, another hook (which runs a command unasked, and may answer a permission),
//!   an `env`, a helper command, loosens, and so does a document that cannot be read: it is judged
//!   by what it could hold;
//! - `--mcp-config`, a document that names only the worker's own tools server, since every other
//!   stdio server is a command run unasked;
//! - `--plugin-dir`, the worker's own mod and no other plugin, whose hooks run unasked.

use std::path::{Path, PathBuf};

use serde_json::Value;
use slopty_proto::project::{PERMISSION_MODE_FLAG, SAFE_FLAGS, SAFE_MODES};

use crate::claude_mod::PLUGIN_DIR_FLAG;
use crate::detect;
use crate::hooks::{MCP_CONFIG_FLAG, MCP_SERVER_NAME};

const SETTINGS_FLAG: &str = "--settings";

/// The settings a `--settings` document may hold and loosen nothing: how Claude Code looks and
/// which model it uses. `hooks`, `statusLine` and `permissions` are judged by what they hold.
const SAFE_SETTINGS: [&str; 14] = [
    "$schema",
    "alwaysThinkingEnabled",
    "cleanupPeriodDays",
    "effortLevel",
    "includeCoAuthoredBy",
    "language",
    "model",
    "outputStyle",
    "showTurnDuration",
    "spinnerTipsEnabled",
    "theme",
    "tui",
    "verbose",
    "viewMode",
];

/// The `permissions` a `--settings` document may hold and loosen nothing: rules that deny or
/// ask, bypass locked off, and the mode it starts in when that mode asks.
const SAFE_PERMISSIONS: [&str; 4] = ["deny", "ask", "disableBypassPermissionsMode", "defaultMode"];

/// What the worker adds to every agent it starts, which is its own and loosens nothing.
///
/// That is the `slopty` its hooks, status line and tools run, and its mod's plugin directory.
/// Nothing is anyone's own where they are not named, as on the server, which adds none of them
/// itself and so takes none from an agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Own {
    /// The `slopty` binary the worker's hooks and tools run.
    pub slopty: Option<PathBuf>,
    /// The directory of the worker's Claude Code mod.
    pub plugin_dir: Option<PathBuf>,
}

impl Own {
    fn is_slopty(&self, command: &str) -> bool {
        self.slopty.as_deref().is_some_and(|own| Path::new(command) == own)
    }
}

/// What in the command line `argv` of a Claude Code process (running in `cwd`) loosens its
/// permissions, as a reader would name it; empty when nothing does.
#[must_use]
pub fn loosening(argv: &[String], cwd: &Path, own: &Own) -> Vec<String> {
    let words = match detect::shell_command(argv) {
        Some(command) => detect::shell_agent_args(command).unwrap_or_default(),
        None => detect::agent_args(argv).to_vec(),
    };
    args(&words, Some(cwd), own)
}

/// What in `claude`'s own arguments `words` loosens its permissions.
///
/// A `--settings` or `--mcp-config` file is read under `cwd`, and counts as loosening when
/// there is none to read it under (the server, which sees no worker's disk).
#[must_use]
pub fn args(words: &[String], cwd: Option<&Path>, own: &Own) -> Vec<String> {
    let mut found = Vec::new();
    let mut words = words.iter().peekable();
    while let Some(word) = words.next() {
        if !word.starts_with('-') {
            continue;
        }
        let (flag, inline) =
            word.split_once('=').map_or((word.as_str(), None), |(f, v)| (f, Some(v)));
        let mut value = || inline.map(str::to_owned).or_else(|| words.next().cloned());
        if flag == PERMISSION_MODE_FLAG {
            let mode = value();
            if !mode.as_deref().is_some_and(|m| SAFE_MODES.contains(&m)) {
                found.push(format!("{PERMISSION_MODE_FLAG} {}", mode.unwrap_or_default()));
            }
        } else if flag == SETTINGS_FLAG {
            found.extend(settings(value().as_deref().unwrap_or_default(), cwd, own));
        } else if flag == PLUGIN_DIR_FLAG {
            let dir = value();
            if own.plugin_dir.as_deref().is_none_or(|d| dir.as_deref() != d.to_str()) {
                found.push(PLUGIN_DIR_FLAG.to_owned());
            }
        } else if flag == MCP_CONFIG_FLAG {
            // Variadic: every value up to the next flag is a document.
            let mut configs: Vec<String> = inline.map(str::to_owned).into_iter().collect();
            while let Some(next) = words.next_if(|w| !w.starts_with('-')) {
                configs.push(next.clone());
            }
            found.extend(configs.iter().filter_map(|c| mcp_config(c, cwd, own)));
        } else if !SAFE_FLAGS.contains(&flag) {
            found.push(flag.to_owned());
        }
    }
    found
}

/// The document a `--settings` or `--mcp-config` value holds: JSON when it opens an object,
/// else a file under `cwd`.
fn document(value: &str, cwd: Option<&Path>) -> Option<Value> {
    let text = if value.trim_start().starts_with('{') {
        value.to_owned()
    } else {
        std::fs::read_to_string(cwd?.join(value)).ok()?
    };
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
}

/// What in the `--settings` value `value` loosens permissions.
fn settings(value: &str, cwd: Option<&Path>, own: &Own) -> Vec<String> {
    let Some(doc) = document(value, cwd) else {
        return vec![format!("{SETTINGS_FLAG} {value}, which cannot be read here")];
    };
    let mut found = Vec::new();
    for (key, held) in doc.as_object().into_iter().flatten() {
        match key.as_str() {
            "permissions" => found.extend(permissions(held)),
            "hooks" => found.extend(hooks(held, own)),
            "statusLine" if !is_own_status_line(held, own) => {
                found.push(format!("{SETTINGS_FLAG} with a status line command of its own"));
            }
            "statusLine" => {}
            key if SAFE_SETTINGS.contains(&key) => {}
            key => found.push(format!("{SETTINGS_FLAG} with {key}")),
        }
    }
    found
}

fn permissions(held: &Value) -> Vec<String> {
    let Some(held) = held.as_object() else {
        return vec![format!("{SETTINGS_FLAG} with permissions it cannot read")];
    };
    let mut found = Vec::new();
    for (key, value) in held {
        match key.as_str() {
            "defaultMode" if !value.as_str().is_some_and(|m| SAFE_MODES.contains(&m)) => {
                let mode = value.as_str().unwrap_or("?");
                found.push(format!("{SETTINGS_FLAG} starting in {mode} (permissions.defaultMode)"));
            }
            key if SAFE_PERMISSIONS.contains(&key) => {}
            key => found.push(format!("{SETTINGS_FLAG} with permissions.{key}")),
        }
    }
    found
}

/// Every hook a document registers must be the worker's own relay: any other runs a command
/// unasked, and one on `PermissionRequest` or `PreToolUse` may answer for the person.
fn hooks(held: &Value, own: &Own) -> Vec<String> {
    let Some(events) = held.as_object() else {
        return vec![format!("{SETTINGS_FLAG} with hooks it cannot read")];
    };
    let mut found = Vec::new();
    for (event, groups) in events {
        let groups = groups.as_array().map(Vec::as_slice);
        let own = groups.is_some_and(|groups| {
            groups.iter().all(|group| {
                let entries = group.get("hooks").and_then(Value::as_array);
                entries.is_some_and(|entries| entries.iter().all(|e| is_own_hook(e, own)))
            })
        });
        if !own {
            found.push(format!("{SETTINGS_FLAG} with a {event} hook of its own"));
        }
    }
    found
}

/// The worker's relay or reports hook: its `slopty`, with `hook` or `hook reports` and nothing
/// that runs anything else.
fn is_own_hook(entry: &Value, own: &Own) -> bool {
    let command = entry.get("command").and_then(Value::as_str).unwrap_or_default();
    let args = entry.get("args").and_then(Value::as_array).map(Vec::as_slice);
    let words = matches!(args, Some([hook]) if hook == "hook")
        || matches!(args, Some([hook, reports]) if hook == "hook" && reports == "reports");
    own.is_slopty(command) && words
}

/// The worker's status line wrapper (`<slopty> hook statusline`), carrying no command of the
/// document's own.
fn is_own_status_line(held: &Value, own: &Own) -> bool {
    let command = held.get("command").and_then(Value::as_str).unwrap_or_default();
    detect::simple_command(command).is_some_and(|words| match words.as_slice() {
        [program, hook, statusline] => {
            own.is_slopty(program) && hook == "hook" && statusline == "statusline"
        }
        _ => false,
    })
}

/// What in an `--mcp-config` value loosens: any server but the worker's own tools.
fn mcp_config(value: &str, cwd: Option<&Path>, own: &Own) -> Option<String> {
    let Some(doc) = document(value, cwd) else {
        return Some(format!("{MCP_CONFIG_FLAG} {value}, which cannot be read here"));
    };
    let servers = doc.get("mcpServers").and_then(Value::as_object);
    let ours = servers.is_some_and(|servers| {
        servers.iter().all(|(name, server)| {
            let command = server.get("command").and_then(Value::as_str).unwrap_or_default();
            let args = server.get("args").and_then(Value::as_array).map(Vec::as_slice);
            name == MCP_SERVER_NAME
                && own.is_slopty(command)
                && matches!(args, Some([mcp]) if mcp == "mcp")
                && server.as_object().is_some_and(|s| {
                    s.keys().all(|k| matches!(k.as_str(), "type" | "command" | "args"))
                })
        })
    });
    let only_servers = doc.as_object().is_some_and(|d| d.keys().all(|k| k == "mcpServers"));
    (!ours || !only_servers).then(|| format!("{MCP_CONFIG_FLAG} with a server of its own"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Where the worker's own `slopty` is in these tests: a path that needs quoting.
    const SLOPTY: &str = "/Apps/It's here/slopty";

    fn own() -> Own {
        Own { slopty: Some(PathBuf::from(SLOPTY)), plugin_dir: Some(PathBuf::from("/mod")) }
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn loose(argv: &[String], cwd: &Path) -> Vec<String> {
        loosening(argv, cwd, &own())
    }

    /// Flags that loosen are found however `claude` was started: bare, under a runtime, or in
    /// a shell's command. Only flags known to loosen nothing, and modes that ask, pass; a flag
    /// after `--`, or any unknown one, does not.
    #[test]
    fn loose_flags_are_found_however_claude_was_started() {
        let cwd = Path::new("/");
        assert_eq!(loose(&words("claude --allowedTools Bash"), cwd), ["--allowedTools"]);
        assert_eq!(
            loose(&words("node /opt/claude-code/cli.js --dangerously-skip-permissions"), cwd),
            ["--dangerously-skip-permissions"]
        );
        let wrapped =
            vec!["sh".to_owned(), "-c".to_owned(), "claude '--allowed-tools=Edit' -c".to_owned()];
        assert_eq!(loose(&wrapped, cwd), ["--allowed-tools"]);
        assert_eq!(
            loose(&words("claude --permission-mode=bypassPermissions"), cwd),
            ["--permission-mode bypassPermissions"]
        );
        assert_eq!(
            loose(&words("claude --permission-mode plan --model opus"), cwd),
            Vec::<String>::new()
        );
        assert_eq!(loose(&words("claude --permission-mode manual"), cwd), Vec::<String>::new());
        assert_eq!(
            loose(&words("claude --append-system-prompt -- --allowedTools Bash"), cwd),
            ["--", "--allowedTools"],
            "past a `--`, parsed by rules not repeated here"
        );
        assert_eq!(
            loose(&words("claude --add-dir / --bare --tmux --future-flag"), cwd),
            ["--add-dir", "--bare", "--tmux", "--future-flag"]
        );
        let known = words("claude -c --model opus --effort=high -n x --resume abc");
        assert!(loose(&known, cwd).is_empty(), "flags known to loosen nothing, and their values");
        assert!(loose(&words("claude --plugin-dir=/mod"), cwd).is_empty(), "the worker's mod");
        assert_eq!(loose(&words("claude --plugin-dir /tmp/p"), cwd), ["--plugin-dir"]);
    }

    /// A shell line reads as the shell would run it: the flags of the `claude` in it, wherever
    /// it stands in the line, unquoted; another program's flags, or a flag only mentioned, are
    /// not the agent's.
    #[test]
    fn a_shell_wrapped_claude_is_read_as_the_shell_runs_it() {
        let cwd = Path::new("/");
        let sh = |line: &str| vec!["/bin/zsh".to_owned(), "-lic".to_owned(), line.to_owned()];
        let chained = sh("cd ~/src && FOO=1 exec claude --allowedTools 'Bash(rm:*)'");
        assert_eq!(loose(&chained, cwd), ["--allowedTools"]);
        let quoted = sh(r#"claude "--permission-mode" "bypass"'Permissions'"#);
        assert_eq!(loose(&quoted, cwd), ["--permission-mode bypassPermissions"]);
        assert_eq!(
            loose(&sh("grep --allowedTools notes.md; claude -c"), cwd),
            Vec::<String>::new()
        );
        assert_eq!(
            loose(&sh("echo claude --dangerously-skip-permissions"), cwd),
            Vec::<String>::new()
        );
        // What the worker's own start looks like through a login shell: its settings, quoted
        // as `slopty_core::shell_quote` quotes them, loosen nothing.
        let ours = json!({
            "hooks": { "Stop": [{ "hooks": [
                { "type": "command", "command": SLOPTY, "args": ["hook"] },
                { "type": "command", "command": SLOPTY, "args": ["hook", "reports"] },
            ]}]},
            "permissions": { "disableBypassPermissionsMode": "disable" },
        })
        .to_string();
        let line = ["claude", "--permission-mode", "default", "--settings", ours.as_str()]
            .map(slopty_core::shell_quote)
            .join(" ");
        assert!(loose(&sh(&line), cwd).is_empty(), "{line}");
        let allowing = json!({ "permissions": { "allow": ["Bash"] } }).to_string();
        let line =
            ["claude", "--settings", allowing.as_str()].map(slopty_core::shell_quote).join(" ");
        assert_eq!(loose(&sh(&line), cwd).len(), 1, "{line}");
    }

    /// The worker's own `--settings` (its hooks and status line, bypass locked off) loosens
    /// nothing. One that allows tools, starts loose, carries an environment or a helper, or
    /// registers any hook or status line of its own does, and so does one that cannot be read.
    #[test]
    fn a_settings_document_loosens_by_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let argv =
            |doc: &Value| vec!["claude".to_owned(), "--settings".to_owned(), doc.to_string()];
        let wrapper = format!("{} hook statusline", slopty_core::shell_quote(SLOPTY));
        let ours = json!({
            "hooks": { "PermissionRequest": [{ "hooks": [
                { "type": "command", "command": SLOPTY, "args": ["hook"] },
            ]}]},
            "statusLine": { "type": "command", "command": wrapper, "padding": 0 },
            "permissions": { "disableBypassPermissionsMode": "disable", "deny": ["Bash(rm:*)"] },
            "model": "opus",
        });
        assert_eq!(loose(&argv(&ours), cwd), Vec::<String>::new());
        let judged = |doc: Value| loose(&argv(&doc), cwd);
        assert_eq!(
            judged(json!({ "permissions": { "allow": ["Bash(rm:*)"] } })),
            ["--settings with permissions.allow"]
        );
        assert!(
            judged(json!({ "permissions": { "defaultMode": "acceptEdits" } }))[0]
                .contains("acceptEdits")
        );
        assert_eq!(
            judged(json!({ "env": { "CLAUDE_CONFIG_DIR": "/tmp/x" } })),
            ["--settings with env"]
        );
        assert_eq!(
            judged(json!({ "apiKeyHelper": "/tmp/x.sh" })).len(),
            1,
            "a command run unasked"
        );
        let hook = |command: &str, args: Value| {
            json!({ "hooks": { "SessionStart": [{ "hooks": [
                { "type": "command", "command": command, "args": args },
            ]}]}})
        };
        assert!(judged(hook("curl evil", json!([])))[0].contains("SessionStart"));
        assert_eq!(judged(hook("/tmp/x/slopty", json!(["hook"]))).len(), 1, "posing as ours");
        let status = |command: String| json!({ "statusLine": { "command": command } });
        assert_eq!(judged(status(format!("{wrapper} --command evil"))).len(), 1, "carrying one");
        assert_eq!(judged(status(format!("evil; {wrapper}"))).len(), 1, "chaining one");

        let allowing = json!({ "permissions": { "allow": ["Bash"] } }).to_string();
        std::fs::write(cwd.join("s.json"), allowing).unwrap();
        assert_eq!(loose(&words("claude --settings=s.json"), cwd).len(), 1, "read from its cwd");
        assert!(loose(&words("claude --settings missing.json"), cwd)[0].contains("cannot be read"));
        let server = args(&words("--settings=s.json"), None, &Own::default());
        assert!(!server.is_empty(), "no disk to read it on");
        let ours_on_server =
            args(&["--settings".to_owned(), ours.to_string()], None, &Own::default());
        assert!(!ours_on_server.is_empty(), "the server takes no hooks from an agent");
    }

    /// The worker's own tools server loosens nothing; any other server is a command run unasked,
    /// in any of the flag's values.
    #[test]
    fn an_mcp_config_loosens_unless_it_names_only_the_workers_tools() {
        let cwd = Path::new("/");
        let ours = crate::hooks::mcp_config(SLOPTY).to_string();
        let argv = |values: &[&str]| {
            let mut argv = vec!["claude".to_owned(), "--mcp-config".to_owned()];
            argv.extend(values.iter().map(|v| (*v).to_owned()));
            argv.extend(["--model".to_owned(), "opus".to_owned()]);
            argv
        };
        assert_eq!(loose(&argv(&[&ours]), cwd), Vec::<String>::new());
        assert_eq!(
            loose(&["claude".to_owned(), format!("--mcp-config={ours}")], cwd),
            Vec::<String>::new()
        );
        let other = json!({ "mcpServers": { "x": { "command": "/tmp/x" } } }).to_string();
        assert_eq!(loose(&argv(&[&ours, &other]), cwd), ["--mcp-config with a server of its own"]);
        let posing = crate::hooks::mcp_config("/tmp/slopty").to_string();
        assert_eq!(loose(&argv(&[&posing]), cwd).len(), 1, "another slopty");
        let more = json!({ "mcpServers": { "slopty": {
            "command": SLOPTY, "args": ["mcp"], "env": { "PATH": "/tmp" },
        }}})
        .to_string();
        assert_eq!(loose(&argv(&[&more]), cwd).len(), 1, "ours with more than its name");
    }
}
