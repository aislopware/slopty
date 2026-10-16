//! Slopty's tools for an agent it starts: `slopty mcp` on stdio, handed to Claude Code for one run
//! with `--mcp-config`, as far as this machine's managed settings let them in.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::managed::ManagedSettings;

/// The `slopty` CLI shipped beside the running binary: in a bundle every binary lives in
/// `Contents/MacOS`, and in a build tree in `target/<profile>`.
#[must_use]
pub fn cli_beside_this_binary() -> Option<PathBuf> {
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
/// [`slopty_proto::ctl::SERVER_ENV`], which the agent passes on to its MCP servers.
#[must_use]
pub fn mcp_config(command: &str) -> Value {
    json!({
        "mcpServers": {
            MCP_SERVER_NAME: { "type": "stdio", "command": command, "args": ["mcp"] }
        }
    })
}

/// `claude` arguments that also serve Slopty's tools through `<command> mcp`, for that run
/// alone, as far as this machine's managed settings let them ([`with_mcp_under`]).
#[must_use]
pub fn with_mcp(args: Vec<String>, command: &str) -> Vec<String> {
    with_mcp_under(args, command, &ManagedSettings::current())
}

/// [`with_mcp`] under `managed`.
///
/// The caller's own `--mcp-config`s are kept, as Claude Code merges every one it is given.
/// Where the managed settings would refuse the run for the flag (sideloading off) or drop the
/// server (an MCP list, or MCP from plugins only), `args` stay as they are.
#[must_use]
pub fn with_mcp_under(args: Vec<String>, command: &str, managed: &ManagedSettings) -> Vec<String> {
    if !admits_tools(command, managed) {
        return args;
    }
    let flag = format!("{MCP_CONFIG_FLAG}={}", mcp_config(command));
    std::iter::once(flag).chain(args).collect()
}

/// Whether `managed` lets Slopty's tools, served by `<command> mcp`, into a run.
fn admits_tools(command: &str, managed: &ManagedSettings) -> bool {
    managed.admits_mcp(MCP_SERVER_NAME, &[command, "mcp"])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slopty's tools reach an agent it starts through `slopty mcp`, in the flag's `=` form so
    /// a prompt after it stays a prompt, and the caller's own MCP servers stay beside them.
    #[test]
    fn a_started_agent_is_handed_slopty_mcp() {
        let args = with_mcp_under(
            vec!["--mcp-config".to_owned(), "mine.json".to_owned(), "fix it".to_owned()],
            "/opt/slopty",
            &ManagedSettings::default(),
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

    /// Under managed settings that refuse a run with `--mcp-config` (sideloading off) or keep
    /// Slopty's server out, the arguments stay as they were given.
    #[test]
    fn managed_settings_keep_the_tools_out() {
        let words = |args: &[&str]| args.iter().map(|&a| a.to_owned()).collect::<Vec<_>>();
        for policy in [
            json!({ "disableSideloadFlags": true }),
            json!({ "deniedMcpServers": [{ "serverName": "slopty" }] }),
            json!({ "allowedMcpServers": [{ "serverName": "github" }] }),
            json!({ "strictPluginOnlyCustomization": ["mcp"] }),
        ] {
            let mut managed = ManagedSettings::default();
            managed.add(&policy);
            let plain = with_mcp_under(words(&["x"]), "/opt/Slopty/slopty", &managed);
            assert_eq!(plain, words(&["x"]), "{policy}");
        }
    }
}
