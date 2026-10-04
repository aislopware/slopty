//! The slash commands an agent takes, for the conversation face's composer menu.
//!
//! Two sources:
//! - **Claude Code's own list**, every command the person can run now, built-in, plugin, the
//!   person's and the project's, and MCP prompts alike, in the typeahead's order. The mod sends it
//!   (`$.command.list()`, [`crate::live::Catalog`]) where it is heard. It is the menu there:
//!   nothing is compiled in, so a new Claude Code's commands come with it.
//! - **Custom commands**, read from where Claude Code loads them ([`custom`]): the person's
//!   `~/.claude/commands` and `~/.claude/skills`, the project's `.claude/commands` and
//!   `.claude/skills` in the agent's directory and each one above it short of the home directory,
//!   and every enabled plugin's `commands` and `skills`.
//!
//! A command file's name is the command (`.claude/commands/git/commit.md` is `git:commit`), and
//! its front matter's `description` and `argument-hint` say what it does and takes; with no
//! description, its first line of text does. A skill is its `SKILL.md`'s `name` (else its
//! directory's), unless it says `user-invocable: false`. A plugin's are named
//! `<plugin>:<name>`. The first of a name wins, in the order project, personal, plugin, so the
//! menu lists each name once. They give Claude Code's list their argument hints and where each
//! comes from, which it does not say, and stand alone where the mod is not heard ([`listed`]).

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use slopty_proto::conversation::{CommandSource, SlashCommand};

use crate::live::CommandInfo;

/// Most bytes of a command or skill file read for its front matter and first line.
const HEAD_BYTES: u64 = 8 * 1024;

/// Most custom commands listed: a directory of thousands is not a menu.
const MAX_CUSTOM: usize = 1_000;

/// How deep a `commands` directory's namespaces go.
const MAX_DEPTH: usize = 4;

/// Longest description kept, in characters.
const DESCRIPTION_CHARS: usize = 160;

/// The menu: Claude Code's own list (`agent`) where the mod sent one, each command with the
/// argument hint and the source its file on disk gives (`custom`); else the custom commands
/// alone. Each name once.
#[must_use]
pub fn listed(agent: Option<&[CommandInfo]>, custom: &[SlashCommand]) -> Vec<SlashCommand> {
    let mut seen = HashSet::new();
    let Some(agent) = agent.filter(|a| !a.is_empty()) else {
        return custom.iter().filter(|c| seen.insert(c.name.clone())).cloned().collect();
    };
    agent
        .iter()
        .filter(|c| seen.insert(c.name.clone()))
        .map(|c| {
            let own = custom.iter().find(|o| o.name == c.name);
            SlashCommand {
                name: c.name.clone(),
                description: cut(c.description.trim()),
                argument_hint: own.and_then(|o| o.argument_hint.clone()),
                source: own.map_or_else(|| source_of(&c.source), |o| o.source),
            }
        })
        .collect()
}

/// Where Claude Code says a command comes from, as the menu ranks it. Its `user` is the
/// person's or the project's file, which a file found on disk tells apart; one not found is
/// taken as the person's. An MCP server's prompt ranks as a plugin's: both are added to
/// Claude Code rather than written by the person.
fn source_of(said: &str) -> CommandSource {
    match said {
        "builtin" => CommandSource::BuiltIn,
        "user" => CommandSource::Personal,
        _ => CommandSource::Plugin,
    }
}

/// The custom commands and skills Claude Code would load for an agent in `cwd`, in the order
/// the module doc gives; a name may repeat.
#[must_use]
pub fn custom(home: &Path, cwd: &Path) -> Vec<SlashCommand> {
    let mut out = Vec::new();
    for dir in project_dirs(home, cwd) {
        let claude = dir.join(".claude");
        commands_in(&claude.join("commands"), None, CommandSource::Project, &mut out);
        skills_in(&claude.join("skills"), None, CommandSource::Project, &mut out);
    }
    let personal = home.join(".claude");
    commands_in(&personal.join("commands"), None, CommandSource::Personal, &mut out);
    skills_in(&personal.join("skills"), None, CommandSource::Personal, &mut out);
    for (plugin, root) in plugins(home, cwd) {
        commands_in(&root.join("commands"), Some(&plugin), CommandSource::Plugin, &mut out);
        skills_in(&root.join("skills"), Some(&plugin), CommandSource::Plugin, &mut out);
    }
    out.truncate(MAX_CUSTOM);
    out
}

/// `cwd` and each directory above it, nearest first, short of `home` and the root: where a
/// project's `.claude` can be.
fn project_dirs(home: &Path, cwd: &Path) -> Vec<PathBuf> {
    cwd.ancestors()
        .take_while(|dir| *dir != home && dir.parent().is_some())
        .map(Path::to_path_buf)
        .collect()
}

/// The command files under `dir`, namespaced by their subdirectories.
fn commands_in(
    dir: &Path,
    plugin: Option<&str>,
    source: CommandSource,
    out: &mut Vec<SlashCommand>,
) {
    fn walk(dir: &Path, prefix: &str, depth: usize, found: &mut Vec<(String, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&path, &format!("{prefix}{name}:"), depth.saturating_add(1), found);
                }
            } else if let Some(stem) = name.strip_suffix(".md") {
                found.push((format!("{prefix}{stem}"), path));
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, "", 0, &mut found);
    for (name, path) in found {
        let Some(head) = head_of(&path) else { continue };
        let (fields, body) = front_matter(&head);
        let description =
            field(&fields, "description").map_or_else(|| first_line(body), str::to_owned);
        out.push(SlashCommand {
            name: named(plugin, &name),
            description: cut(&description),
            argument_hint: field(&fields, "argument-hint").map(str::to_owned),
            source,
        });
    }
}

/// The skills under `dir`, one directory each with its `SKILL.md`.
fn skills_in(dir: &Path, plugin: Option<&str>, source: CommandSource, out: &mut Vec<SlashCommand>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let Some(head) = head_of(&entry.path().join("SKILL.md")) else { continue };
        let (fields, body) = front_matter(&head);
        if field(&fields, "user-invocable") == Some("false") {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().into_owned();
        let name = field(&fields, "name").map_or(folder, str::to_owned);
        let description =
            field(&fields, "description").map_or_else(|| first_line(body), str::to_owned);
        out.push(SlashCommand {
            name: named(plugin, &name),
            description: cut(&description),
            argument_hint: field(&fields, "argument-hint").map(str::to_owned),
            source,
        });
    }
}

/// The enabled plugins' names and install directories: installed as
/// `~/.claude/plugins/installed_plugins.json` records them, and enabled by the person's
/// settings or the project's, the project's last.
fn plugins(home: &Path, cwd: &Path) -> Vec<(String, PathBuf)> {
    let read = |path: PathBuf| -> Option<Value> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    };
    let mut enabled = std::collections::HashMap::new();
    let settings = [
        home.join(".claude/settings.json"),
        cwd.join(".claude/settings.json"),
        cwd.join(".claude/settings.local.json"),
    ];
    for file in settings {
        let Some(Value::Object(plugins)) =
            read(file).and_then(|s| s.get("enabledPlugins").cloned())
        else {
            continue;
        };
        for (key, on) in plugins {
            enabled.insert(key, on.as_bool().unwrap_or(false));
        }
    }
    let Some(Value::Object(installed)) = read(home.join(".claude/plugins/installed_plugins.json"))
        .and_then(|i| i.get("plugins").cloned())
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = installed
        .into_iter()
        .filter(|(key, _)| enabled.get(key).copied().unwrap_or(false))
        .filter_map(|(key, installs)| {
            let path = installs.as_array()?.first()?.get("installPath")?.as_str()?.to_owned();
            let name = key.split_once('@').map_or(key.as_str(), |(name, _)| name).to_owned();
            Some((name, PathBuf::from(path)))
        })
        .collect();
    out.sort();
    out
}

/// A command's name under its plugin, if it has one.
fn named(plugin: Option<&str>, name: &str) -> String {
    match plugin {
        Some(plugin) => format!("{plugin}:{name}"),
        None => name.to_owned(),
    }
}

/// The first [`HEAD_BYTES`] of a file as text; `None` when it cannot be read.
fn head_of(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(HEAD_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A Markdown file's front matter as `key: value` pairs (a value's quotes taken off), and the
/// text after it. Only the flat keys a command uses are read; a nested block is passed over.
fn front_matter(text: &str) -> (Vec<(&str, &str)>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return (Vec::new(), text);
    };
    let Some(end) = rest.find("\n---") else { return (Vec::new(), text) };
    let (block, after) = rest.split_at(end);
    let body = after.get(4..).unwrap_or_default();
    let fields = block
        .lines()
        .filter(|line| !line.starts_with([' ', '\t']))
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            (key.trim(), value)
        })
        .filter(|(_, value)| !value.is_empty())
        .collect();
    (fields, body)
}

fn field<'a>(fields: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The first line of text in `body`, a heading's marks taken off.
fn first_line(body: &str) -> String {
    body.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// `text` cut to [`DESCRIPTION_CHARS`].
fn cut(text: &str) -> String {
    if text.chars().count() <= DESCRIPTION_CHARS {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(DESCRIPTION_CHARS.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("a parent")).expect("dirs");
        fs::write(path, text).expect("write");
    }

    /// Commands and skills come from the person's, the project's and the enabled plugins'
    /// directories, named as Claude Code names them and described by their front matter or
    /// first line; a nearer project's wins, a skill kept from the menu stays out, a disabled
    /// plugin adds nothing, and Claude Code's own follow.
    #[test]
    fn custom_commands_are_found_where_claude_code_loads_them() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let repo = home.join("src/repo");
        let cwd = repo.join("app");
        write(
            &home.join(".claude/commands/review.md"),
            "---\ndescription: \"Review the diff\"\nargument-hint: [focus]\n---\nReview it.\n",
        );
        write(
            &home.join(".claude/skills/unslop/SKILL.md"),
            "---\nname: unslop\ndescription: Cut AI tells\n---\n",
        );
        write(
            &home.join(".claude/skills/quiet/SKILL.md"),
            "---\nname: quiet\ndescription: Model only\nuser-invocable: false\n---\n",
        );
        write(
            &repo.join(".claude/commands/git/commit.md"),
            "# Commit the staged work\n\nThen push.\n",
        );
        write(&repo.join(".claude/commands/deploy.md"), "---\ndescription: Far deploy\n---\n");
        write(&cwd.join(".claude/commands/deploy.md"), "---\ndescription: Near deploy\n---\n");
        write(&home.join(".claude/commands/compact.md"), "---\ndescription: My compact\n---\n");
        let on = tmp.path().join("plugins/cf");
        let off = tmp.path().join("plugins/lsp");
        write(&on.join("commands/build-agent.md"), "---\ndescription: Build an agent\n---\n");
        write(
            &on.join("skills/wrangler/SKILL.md"),
            "---\nname: wrangler\ndescription: Use wrangler\n---\n",
        );
        write(&off.join("commands/hover.md"), "---\ndescription: Hover\n---\n");
        write(
            &home.join(".claude/plugins/installed_plugins.json"),
            &serde_json::json!({ "version": 2, "plugins": {
                "cloudflare@cloudflare": [{ "installPath": on }],
                "lsp@official": [{ "installPath": off }],
            }})
            .to_string(),
        );
        write(
            &home.join(".claude/settings.json"),
            r#"{ "enabledPlugins": { "cloudflare@cloudflare": true, "lsp@official": false } }"#,
        );

        let own = custom(&home, &cwd);
        let listed = listed(None, &own);
        let find = |name: &str| listed.iter().find(|c| c.name == name);
        let said = |name: &str| find(name).map(|c| (c.description.as_str(), c.source));
        assert_eq!(said("deploy"), Some(("Near deploy", CommandSource::Project)), "nearest wins");
        assert_eq!(
            said("git:commit"),
            Some(("Commit the staged work", CommandSource::Project)),
            "a subdirectory namespaces, and the first line describes"
        );
        assert_eq!(said("review"), Some(("Review the diff", CommandSource::Personal)));
        assert_eq!(find("review").and_then(|c| c.argument_hint.as_deref()), Some("[focus]"));
        assert_eq!(said("unslop"), Some(("Cut AI tells", CommandSource::Personal)));
        assert_eq!(said("quiet"), None, "not the person's to type");
        assert_eq!(said("cloudflare:build-agent"), Some(("Build an agent", CommandSource::Plugin)));
        assert_eq!(said("cloudflare:wrangler"), Some(("Use wrangler", CommandSource::Plugin)));
        assert_eq!(said("lsp:hover"), None, "a disabled plugin");
        assert_eq!(said("compact"), Some(("My compact", CommandSource::Personal)));
        assert_eq!(listed.iter().filter(|c| c.name == "deploy").count(), 1, "each name once");
        assert_eq!(said("model"), None, "nothing of Claude Code's own is compiled in");
    }

    /// Where the mod sent Claude Code's list, it is the menu, in its order: a command with a
    /// file on disk takes the file's argument hint and source, the rest are ranked by what
    /// Claude Code says of them, and a long description is cut. With an empty list the
    /// custom commands stand alone.
    #[test]
    fn claude_codes_own_list_is_the_menu_with_the_disks_hints() {
        let info = |name: &str, description: &str, source: &str| CommandInfo {
            name: name.to_owned(),
            description: description.to_owned(),
            source: source.to_owned(),
        };
        let own = vec![SlashCommand {
            name: "review".to_owned(),
            description: "Review the diff".to_owned(),
            argument_hint: Some("[focus]".to_owned()),
            source: CommandSource::Project,
        }];
        let long = "x".repeat(400);
        let agent = [
            info("model", "Set the AI model", "builtin"),
            info("review", "Review the diff", "user"),
            info("deploy", &long, "user"),
            info("github:pr", "Open a PR", "mcp"),
            info("model", "again", "builtin"),
        ];
        let listed = listed(Some(&agent), &own);
        let names: Vec<&str> = listed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["model", "review", "deploy", "github:pr"]);
        let review = &listed[1];
        assert_eq!(
            (review.source, review.argument_hint.as_deref()),
            (CommandSource::Project, Some("[focus]"))
        );
        assert_eq!(listed[0].source, CommandSource::BuiltIn);
        assert_eq!(listed[2].source, CommandSource::Personal);
        assert_eq!(listed[2].description.chars().count(), DESCRIPTION_CHARS);
        assert_eq!(listed[3].source, CommandSource::Plugin);
        assert_eq!(super::listed(Some(&[]), &own), own, "an empty list is no list");
    }
}
