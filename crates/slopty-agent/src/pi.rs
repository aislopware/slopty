//! pi, its own TUI in a Slopty terminal (`docs/decisions/agents.md`, "TUI-first"), with Slopty's
//! tools registered by a small extension ([`TOOLS`]).

pub mod sessions;

use std::io;
use std::path::{Path, PathBuf};

/// The variable that hands the tools extension ([`TOOLS`]) Slopty's tools to register.
///
/// Its value is an `mcp.json` server entry in JSON ([`tools`]). pi connects a server an extension
/// registers (`pi.registerMcpServer`, pi's `docs/extensions.md`, "MCP servers") as one of its own.
pub const TOOLS_ENV: &str = "SLOPTY_PI_TOOLS";

/// The MCP server entry that serves Slopty's tools through `<relay> mcp` on stdio, with `env`
/// (a seat's variables) as its environment, in the shape of an `mcp.json` `mcpServers` entry.
#[must_use]
pub fn tools(relay: &str, env: &[(String, String)]) -> serde_json::Value {
    let env: serde_json::Map<String, serde_json::Value> =
        env.iter().map(|(n, v)| (n.clone(), serde_json::Value::String(v.clone()))).collect();
    serde_json::json!({ "command": relay, "args": ["mcp"], "env": env })
}

/// The flag that loads an extension.
pub const EXTENSION_FLAG: &str = "--extension";

/// The tools extension's one file: it registers Slopty's tools ([`TOOLS_ENV`]) and nothing
/// else, for a pi Slopty starts in a terminal.
pub const TOOLS: (&str, &str) = ("tools.ts", include_str!("../assets/pi-tools/tools.ts"));

/// Write the tools extension under `data_dir` (`pi-tools/<digest>/tools.ts`) and return the
/// file, under its digest.
///
/// A file already there whole is left as it is. A partial write never takes the name: the file
/// goes to a sibling that is renamed into place.
///
/// # Errors
///
/// When the directory cannot be written.
pub fn install_tools(data_dir: &Path) -> io::Result<PathBuf> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TOOLS.1.as_bytes());
    let digest: String = hasher.finalize().to_hex().as_str().chars().take(16).collect();
    let root = data_dir.join("pi-tools");
    let file = root.join(&digest).join(TOOLS.0);
    if std::fs::read_to_string(&file).is_ok_and(|have| have == TOOLS.1) {
        return Ok(file);
    }
    std::fs::create_dir_all(root.join(&digest))?;
    let staging = root.join(format!(".{digest}-{}", std::process::id()));
    std::fs::write(&staging, TOOLS.1)?;
    std::fs::rename(&staging, &file)?;
    Ok(file)
}

/// The arguments of pi's own TUI: session `resumed` taken up again, else a new one under
/// `session`; `model` when the start named one; the tools extension at `tools`; the first
/// prompt last, after `--`.
#[must_use]
pub fn tui(
    resumed: Option<&str>,
    session: &str,
    model: Option<&str>,
    tools: Option<&Path>,
    prompt: Option<&str>,
) -> Vec<String> {
    let mut args = match resumed {
        Some(id) => sessions::resume_args(id),
        None => vec!["--session-id".to_owned(), session.to_owned()],
    };
    if let Some(tools) = tools {
        args.extend([EXTENSION_FLAG.to_owned(), tools.display().to_string()]);
    }
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
        args.push(prompt.to_owned());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slopty's tools for pi are an `mcp.json` server entry over `slopty mcp`, the seat's
    /// variables its environment; the extension registers what it is handed.
    #[test]
    fn the_tools_are_an_mcp_server_entry_the_extension_registers() {
        let env = [("SLOPTY_SESSION".to_owned(), "seat-1".to_owned())];
        assert_eq!(
            tools("/bin/slopty", &env),
            serde_json::json!({"command": "/bin/slopty", "args": ["mcp"],
                "env": {"SLOPTY_SESSION": "seat-1"}})
        );
        assert!(TOOLS.1.contains(&format!("process.env.{TOOLS_ENV}")), "the extension reads it");
        assert!(TOOLS.1.contains("pi.registerMcpServer("));
    }

    /// Written once, read back whole; a second install finds it; a damaged file is rewritten,
    /// and no staging file is left behind.
    #[test]
    fn the_tools_extension_is_written_under_its_digest() {
        let data = tempfile::tempdir().expect("a temp dir");
        let file = install_tools(data.path()).expect("installed");
        assert_eq!(std::fs::read_to_string(&file).expect("written"), TOOLS.1);
        assert_eq!(install_tools(data.path()).expect("again"), file);
        std::fs::write(&file, "// damaged\n").expect("damage");
        assert_eq!(install_tools(data.path()).expect("repaired"), file);
        assert_eq!(std::fs::read_to_string(&file).expect("read"), TOOLS.1);
        let names: Vec<_> = std::fs::read_dir(data.path().join("pi-tools"))
            .expect("listed")
            .map(|e| e.expect("an entry").file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    /// A TUI opens its session, loads the tools, takes the model only when named, and has its
    /// first prompt last.
    #[test]
    fn a_tui_runs_its_session_with_the_tools_and_the_prompt_last() {
        let tools = Path::new("/data/pi-tools/0123/tools.ts");
        let got = tui(None, "0190-ab", Some(" "), Some(tools), Some("Fix it"));
        let want =
            ["--session-id", "0190-ab", "--extension", "/data/pi-tools/0123/tools.ts", "Fix it"];
        assert_eq!(got, want);
        assert_eq!(
            tui(None, "0190-ab", Some("m"), None, None),
            ["--session-id", "0190-ab", "--model", "m"]
        );
    }
}
