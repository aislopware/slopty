//! `cargo xtask fixtures claude-commands`: Claude Code's own slash commands as the official build
//! defines them, for `slopty_agent::commands`'s list of the ones that open a dialog in the TUI.
//!
//! The plugin API's command list says only a command's name, description and source, so whether
//! one renders in the terminal is read from the build itself: its bundled JavaScript defines each
//! command as an object literal (`{type:"local-jsx",name:"config",…}`). A command the TUI and a
//! headless session both run is defined twice, a `local` one for headless use and a `local-jsx`
//! one for the TUI; the TUI's is the one kept. Nothing runs: the binary is only read.
//!
//! Written to `crates/slopty-agent/tests/fixtures/claude/commands.json`, one entry per command,
//! by name.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};

use crate::tools::repo_root;

/// Where the table goes.
const OUT: &str = "crates/slopty-agent/tests/fixtures/claude/commands.json";

/// How far around a `type:` an object literal is looked for, in bytes.
const REACH: usize = 3000;

/// One definition of a command, as read.
#[derive(Clone, Debug, Default)]
struct Definition {
    kind: String,
    aliases: Vec<String>,
    hidden: bool,
    tui_only: bool,
    argument_hint: Option<String>,
    immediate: &'static str,
}

/// Read the `version` build and write its commands' table.
pub fn record(version: &str) -> Result<()> {
    let claude = crate::claude::official(version)?;
    let bytes = std::fs::read(&claude).with_context(|| format!("read {}", claude.display()))?;
    let table = table(&bytes);
    ensure!(
        table.len() > 20 && table.contains_key("config") && table.contains_key("help"),
        "read only {} commands from {}; has the build's shape changed?",
        table.len(),
        claude.display()
    );
    let commands: Vec<Value> = table
        .iter()
        .map(|(name, d)| {
            json!({
                "name": name,
                "aliases": d.aliases,
                "jsx": d.kind == "local-jsx",
                "tui_only": d.tui_only,
                "hidden": d.hidden,
                "argument_hint": d.argument_hint,
                "immediate": d.immediate,
            })
        })
        .collect();
    let doc = json!({ "claude": version, "commands": commands });
    let out = repo_root()?.join(OUT).into_std_path_buf();
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(&doc)?;
    text.push('\n');
    std::fs::write(&out, text)?;
    println!("wrote {OUT} from Claude Code {version}: {} commands", table.len());
    Ok(())
}

/// Every command the build defines, by name: the TUI's definition where there are two.
fn table(bytes: &[u8]) -> BTreeMap<String, Definition> {
    let mut table: BTreeMap<String, Definition> = BTreeMap::new();
    for kind in ["local-jsx", "local", "prompt"] {
        let needle = format!("type:\"{kind}\"");
        for at in find_all(bytes, needle.as_bytes()) {
            let Some(object) = object_around(bytes, at) else { continue };
            let Some(name) = top_string(object, b"name") else { continue };
            if !is_command_name(&name) {
                continue;
            }
            let read = definition(object, kind);
            let keep = table.get(&name).is_none_or(|kept| kept.kind != "local-jsx");
            if keep {
                let tui_only = read.tui_only || table.get(&name).is_some_and(|k| k.tui_only);
                table.insert(name, Definition { tui_only, ..read });
            }
        }
    }
    table
}

/// What one object literal says of its command.
fn definition(object: &[u8], kind: &str) -> Definition {
    let has = |what: &[u8]| find_all(object, what).next().is_some();
    let immediate = if has(b"immediate:!0") {
        "always"
    } else if has(b"immediate:") && has(b".trim()!==\"\"") {
        "with-arguments"
    } else if has(b"immediate:") {
        "other"
    } else {
        "never"
    };
    Definition {
        kind: kind.to_owned(),
        aliases: top_strings(object, b"aliases"),
        hidden: has(b"isHidden:!0") || has(b"(removed)"),
        tui_only: has(b"supportsNonInteractive:!1"),
        argument_hint: top_string(object, b"argumentHint").filter(|h| !h.is_empty()),
        immediate,
    }
}

/// A slash command's name: lower case, digits, `-` and `:`.
fn is_command_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == ':')
}

/// Every place `needle` starts in `hay`.
fn find_all<'a>(hay: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    hay.windows(needle.len()).enumerate().filter(move |(_, w)| *w == needle).map(|(i, _)| i)
}

/// The object literal holding byte `at`: from its `{` to its `}`, within [`REACH`].
fn object_around(bytes: &[u8], at: usize) -> Option<&[u8]> {
    let floor = at.saturating_sub(REACH);
    let mut depth = 0_usize;
    let mut start = None;
    for i in (floor..at).rev() {
        match bytes.get(i)? {
            b'}' => depth = depth.saturating_add(1),
            b'{' if depth == 0 => {
                start = Some(i);
                break;
            }
            b'{' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let start = start?;
    let ceiling = bytes.len().min(at.saturating_add(REACH));
    let mut depth = 0_usize;
    for i in start..ceiling {
        match bytes.get(i)? {
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return bytes.get(start..=i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Where `key:` starts at the object's own level (not inside a nested value), after `{` or `,`.
fn top_key(object: &[u8], key: &[u8]) -> Option<usize> {
    let mut depth = 0_usize;
    for (i, byte) in object.iter().enumerate() {
        match byte {
            b'{' | b'[' | b'(' => depth = depth.saturating_add(1),
            b'}' | b']' | b')' => depth = depth.saturating_sub(1),
            _ if depth == 1 => {
                let before = i.checked_sub(1).and_then(|b| object.get(b));
                let rest = object.get(i..)?;
                if matches!(before, Some(b'{' | b','))
                    && rest.starts_with(key)
                    && rest.get(key.len()) == Some(&b':')
                {
                    return Some(i.saturating_add(key.len()).saturating_add(1));
                }
            }
            _ => {}
        }
    }
    None
}

/// The string `key` holds at the object's own level.
fn top_string(object: &[u8], key: &[u8]) -> Option<String> {
    let value = object.get(top_key(object, key)?..)?.strip_prefix(b"\"")?;
    let end = value.iter().position(|b| *b == b'"')?;
    String::from_utf8(value.get(..end)?.to_vec()).ok()
}

/// The strings of the list `key` holds at the object's own level.
fn top_strings(object: &[u8], key: &[u8]) -> Vec<String> {
    let Some(at) = top_key(object, key) else { return Vec::new() };
    let Some(list) = object.get(at..).and_then(|v| v.strip_prefix(b"[")) else {
        return Vec::new();
    };
    let end = list.iter().position(|b| *b == b']').unwrap_or(0);
    let inside = String::from_utf8_lossy(list.get(..end).unwrap_or_default()).into_owned();
    inside
        .split(',')
        .filter_map(|s| s.strip_prefix('"')?.strip_suffix('"'))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command defined for headless use and again for the TUI is read as the TUI's: its type,
    /// its aliases, when it acts at once, and whether it exists only in the TUI.
    #[test]
    fn the_tui_s_definition_of_a_command_is_kept() {
        let bundle = br#"var a={type:"local",name:"config",aliases:["settings"],supportsNonInteractive:!0,isEnabled:()=>ve()},b={type:"local-jsx",name:"config",aliases:["settings"],description:"Open settings",argumentHint:"[key=value]",immediate:(e,n)=>e.trim()!==""||n==="fullscreen",requires:{ink:!0}};var c={description:"Restore",name:"rewind",aliases:["checkpoint","undo"],argumentHint:"",type:"local",supportsNonInteractive:!1};var d={type:"local",name:"agents",description:"(removed) Ask Claude",isHidden:!0};var e={type:"local-jsx",name:"mcp",immediate:!0,load:()=>x({name:"inner"})};"#;
        let table = table(bundle);
        let config = &table["config"];
        assert_eq!(config.kind, "local-jsx");
        assert_eq!(config.aliases, ["settings"]);
        assert_eq!(config.immediate, "with-arguments");
        assert_eq!(config.argument_hint.as_deref(), Some("[key=value]"));
        let rewind = &table["rewind"];
        assert!(rewind.tui_only && rewind.kind == "local" && rewind.argument_hint.is_none());
        assert!(table["agents"].hidden);
        assert_eq!(table["mcp"].immediate, "always");
        assert!(!table.contains_key("inner"), "a nested object's name is not the command's");
    }
}
