//! How far a driven pi goes before its gate asks the person (`docs/decisions/agents.md`, "pi's
//! gate lets through what its mode allows").
//!
//! pi has no permission system, so Slopty's gate is the one it runs under, and its mode is the
//! level the person set: for a project's task, the project's autonomy; for their own thread, the
//! thread's mode, which they switch. The gate lets a call through by these rules alone, and asks
//! the person about every other:
//! - [`ASK`]: reads inside the thread's folder, and Slopty's own tools;
//! - [`EDITS`]: those, and edits and writes inside the folder;
//! - [`OWN`]: every call.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use slopty_proto::thread::Mode;

/// Every call asks but a read inside the folder and Slopty's own tools.
pub const ASK: &str = "ask";
/// Edits and writes inside the folder go too.
pub const EDITS: &str = "edits";
/// Every call goes.
pub const OWN: &str = "own";

/// The prefix of Slopty's own tools as pi names them: the gate registers them as the MCP server
/// `slopty`.
const SLOPTY_TOOLS: &str = "mcp__slopty__";

/// pi's own tools that only read.
const READS: [&str; 4] = ["read", "grep", "find", "ls"];
/// pi's own tools that change files.
const EDITS_TOOLS: [&str; 2] = ["edit", "write"];

/// The modes a driven pi offers, mildest first.
#[must_use]
pub fn modes() -> Vec<Mode> {
    let mode = |id: &str, label: &str, description: &str| Mode {
        id: id.to_owned(),
        label: label.to_owned(),
        description: Some(description.to_owned()),
    };
    vec![
        mode(ASK, "Ask", "Asks before each edit and command; reads in its folder go"),
        mode(EDITS, "Accept edits", "Edits in its folder go; commands ask"),
        mode(OWN, "On its own", "Every call goes without asking"),
    ]
}

/// Whether `mode` is one a driven pi takes.
#[must_use]
pub fn known(mode: &str) -> bool {
    [ASK, EDITS, OWN].contains(&mode)
}

/// Whether the gate lets a call through without asking.
///
/// The call is `tool`'s with `input` (and the tool's own `hints`), in `mode`, for a thread
/// working in `cwd`. An unknown mode lets nothing through on its own: the gate fails closed.
#[must_use]
pub fn lets(mode: &str, (tool, input, hints): (&str, &Value, &Value), cwd: &Path) -> bool {
    if !known(mode) {
        return false;
    }
    if mode == OWN || tool.starts_with(SLOPTY_TOOLS) {
        return true;
    }
    let read_only = READS.contains(&tool)
        || (!tool.starts_with("mcp__") && hints.get("readOnlyHint") == Some(&Value::Bool(true)));
    let edits = mode == EDITS && EDITS_TOOLS.contains(&tool);
    (read_only || edits) && within(input, cwd)
}

/// Whether the path `input` names (`path`, else the folder itself) is inside `cwd`, never
/// climbing out of it.
fn within(input: &Value, cwd: &Path) -> bool {
    let named = input.get("path").or_else(|| input.get("file_path")).and_then(Value::as_str);
    let Some(named) = named.filter(|p| !p.is_empty()) else { return true };
    let path = Path::new(named);
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let path: PathBuf = if path.is_absolute() { path.to_owned() } else { cwd.join(path) };
    !cwd.as_os_str().is_empty() && path.starts_with(cwd)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Each mode lets through what it says and asks about the rest: reads and Slopty's tools in
    /// every mode, edits from Accept edits, commands only on its own. A path out of the folder
    /// always asks, and an unknown mode lets nothing through.
    #[test]
    fn each_mode_lets_through_what_it_says() {
        let cwd = Path::new("/w/demo");
        let none = json!({});
        let call = |tool: &str, input: Value| (tool.to_owned(), input);
        let calls = [
            call("read", json!({ "path": "src/main.rs" })),
            call("ls", json!({})),
            call("read", json!({ "path": "/etc/hosts" })),
            call("read", json!({ "path": "../other/secret" })),
            call("edit", json!({ "path": "/w/demo/src/main.rs" })),
            call("write", json!({ "path": "/tmp/out.txt" })),
            call("bash", json!({ "command": "cargo test" })),
            call("mcp__slopty__task_report", json!({ "note": "done" })),
        ];
        let lets_in = |mode: &str| -> Vec<bool> {
            calls
                .iter()
                .map(|(tool, input)| lets(mode, (tool.as_str(), input, &none), cwd))
                .collect()
        };
        let (y, n) = (true, false);
        assert_eq!(lets_in(ASK), [y, y, n, n, n, n, n, y]);
        assert_eq!(lets_in(EDITS), [y, y, n, n, y, n, n, y]);
        assert_eq!(lets_in(OWN), [y, y, y, y, y, y, y, y]);
        assert_eq!(lets_in("bypass"), [n, n, n, n, n, n, n, n], "fails closed");
    }

    /// A tool that says it only reads goes as a read does, unless it is another MCP server's,
    /// whose word is not taken.
    #[test]
    fn a_read_only_hint_is_taken_from_pi_s_own_tools_alone() {
        let cwd = Path::new("/w/demo");
        let hints = json!({ "readOnlyHint": true });
        assert!(lets(ASK, ("outline", &json!({}), &hints), cwd));
        assert!(!lets(ASK, ("mcp__web__fetch", &json!({}), &hints), cwd));
    }

    /// The modes offered are the three the gate knows, mildest first.
    #[test]
    fn the_modes_offered_are_the_ones_known() {
        let ids: Vec<String> = modes().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, [ASK, EDITS, OWN]);
        assert!(ids.iter().all(|id| known(id)));
    }
}
