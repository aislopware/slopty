//! The slash commands an agent takes, for the conversation face's composer menu.
//!
//! Two kinds:
//! - **Claude Code's own**, from a table read out of the version the fixtures pin ([`BUILT_IN`]).
//!   Its commands are compiled in, and the one list a running Claude Code could give (the mod's
//!   `command.list`) has a shape nobody has checked, so the table is the source until then.
//! - **Custom commands**, read from where Claude Code loads them ([`custom`]): the person's
//!   `~/.claude/commands` and `~/.claude/skills`, the project's `.claude/commands` and
//!   `.claude/skills` in the agent's directory and each one above it short of the home directory,
//!   and every enabled plugin's `commands` and `skills`.
//!
//! A command file's name is the command (`.claude/commands/git/commit.md` is `git:commit`), and
//! its front matter's `description` and `argument-hint` say what it does and takes; with no
//! description, its first line of text does. A skill is its `SKILL.md`'s `name` (else its
//! directory's), unless it says `user-invocable: false`. A plugin's are named
//! `<plugin>:<name>`. The first of a name wins, in the order project, personal, plugin, Claude
//! Code's own, so the menu lists each name once.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use slopty_proto::conversation::{CommandSource, SlashCommand};

/// The Claude Code version [`BUILT_IN`] was read from.
pub const BUILT_IN_VERSION: &str = "2.1.283";

/// Claude Code's own commands and bundled skills a person can type.
///
/// Each is `(name, description, argument hint)`: the ones enabled in an ordinary interactive
/// session of [`BUILT_IN_VERSION`], with the words its `/help` shows.
pub const BUILT_IN: &[(&str, &str, Option<&str>)] = &[
    ("add-dir", "Add a new working directory", Some("<path>")),
    ("background", "Send this session to the background and free the terminal", Some("[prompt]")),
    ("batch", "Plan a large change; background agents each open a PR", None),
    ("branch", "Create a branch of the current conversation at this point", Some("[name]")),
    (
        "btw",
        "Ask a quick side question without interrupting the main conversation",
        Some("[question]"),
    ),
    ("cd", "Move this session to a new working directory", Some("<path>")),
    (
        "clear",
        "Start a new session with empty context; the previous one stays resumable",
        Some("[name]"),
    ),
    ("code-review", "Review the current diff for correctness bugs", None),
    (
        "compact",
        "Free up context by summarizing the conversation so far",
        Some("<optional custom summarization instructions>"),
    ),
    ("config", "Open settings", Some("[key=value]")),
    ("context", "Visualize current context usage as a colored grid", Some("[all]")),
    ("copy", "Copy Claude's last response to clipboard (or /copy N for the Nth-latest)", None),
    ("debug", "Turn on debug logging and investigate problems", Some("[issue description]")),
    ("doctor", "Health-check your setup and fix issues", None),
    ("effort", "Set effort level for model usage", None),
    ("exit", "Exit Claude Code", None),
    ("export", "Export the current conversation to a file or clipboard", Some("[filename]")),
    ("feedback", "Send feedback to Anthropic or report a bug", Some("[report]")),
    ("fewer-permission-prompts", "Add an allowlist for the read-only commands you run most", None),
    ("fork", "Spawn a background agent that inherits the full conversation", Some("<directive>")),
    ("goal", "Set a goal Claude checks before stopping", Some("[<condition> | clear]")),
    ("help", "Show help and available commands", None),
    ("hooks", "View hook configurations for tool events", None),
    ("ide", "Manage IDE integrations and show status", Some("[open]")),
    ("init", "Initialize a new CLAUDE.md file with codebase documentation", None),
    ("insights", "Generate a report analyzing your Claude Code sessions", None),
    ("keybindings", "Open your keyboard shortcuts file", None),
    ("login", "Sign in with your Anthropic account", None),
    ("logout", "Sign out from your Anthropic account", None),
    ("loop", "Run a prompt or slash command on a recurring interval", Some("[interval] <prompt>")),
    ("mcp", "Manage MCP servers", Some("[reconnect <server>|enable|disable [<server>|all]]")),
    ("memory", "Edit CLAUDE.md files and memory settings", None),
    ("model", "Set the AI model for Claude Code", Some("[model]")),
    ("output-style", "List output styles or switch to one", Some("[style]")),
    ("permissions", "Manage allow and deny tool permission rules", None),
    ("plan", "Enable plan mode or view the current session plan", Some("[open|<description>]")),
    ("plugin", "Manage Claude Code plugins", None),
    ("recap", "Generate a one-line session recap now", None),
    ("release-notes", "View release notes", None),
    ("reload-plugins", "Activate pending plugin changes in the current session", Some("[--force]")),
    ("reload-skills", "Pick up skills added or changed on disk during this session", None),
    ("rename", "Rename the current conversation", Some("[name]")),
    ("resume", "Resume a previous conversation", Some("[conversation id or search term]")),
    ("rewind", "Restore the code and/or conversation to a previous point", None),
    (
        "security-review",
        "Complete a security review of the pending changes on the current branch",
        None,
    ),
    ("simplify", "Review the changed code for reuse, simplification and efficiency", None),
    ("skills", "List available skills", None),
    (
        "status",
        "Show Claude Code status: version, model, account, API connectivity and tools",
        None,
    ),
    ("statusline", "Set up Claude Code's status line UI", None),
    (
        "subtask",
        "Send a subagent off with your full context; its result comes back here",
        Some("<task>"),
    ),
    ("tasks", "View and manage everything running in the background", None),
    ("theme", "Change the theme", None),
    ("usage", "Show session cost, plan usage, and activity stats", None),
];

/// Most bytes of a command or skill file read for its front matter and first line.
const HEAD_BYTES: u64 = 8 * 1024;

/// Most custom commands listed: a directory of thousands is not a menu.
const MAX_CUSTOM: usize = 1_000;

/// How deep a `commands` directory's namespaces go.
const MAX_DEPTH: usize = 4;

/// Longest description kept, in characters.
const DESCRIPTION_CHARS: usize = 160;

/// Claude Code's own commands, from [`BUILT_IN`].
pub fn built_in() -> impl Iterator<Item = SlashCommand> {
    BUILT_IN.iter().map(|(name, description, hint)| SlashCommand {
        name: (*name).to_owned(),
        description: (*description).to_owned(),
        argument_hint: hint.map(str::to_owned),
        source: CommandSource::BuiltIn,
    })
}

/// Every command an agent running in `cwd`, for a person whose home is `home`, takes: the
/// custom ones first, then Claude Code's own, each name once.
#[must_use]
pub fn all(home: &Path, cwd: &Path) -> Vec<SlashCommand> {
    let mut seen = HashSet::new();
    custom(home, cwd)
        .into_iter()
        .chain(built_in())
        .filter(|c| seen.insert(c.name.clone()))
        .collect()
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

        let listed = all(&home, &cwd);
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
        assert_eq!(said("compact"), Some(("My compact", CommandSource::Personal)), "custom first");
        assert_eq!(listed.iter().filter(|c| c.name == "compact").count(), 1, "each name once");
        assert_eq!(
            said("model"),
            Some(("Set the AI model for Claude Code", CommandSource::BuiltIn))
        );
    }

    /// Claude Code's table names each command once.
    #[test]
    fn the_built_in_table_names_each_command_once() {
        let mut names: Vec<&str> = BUILT_IN.iter().map(|(name, ..)| *name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
        assert!(names.contains(&"rewind") && names.contains(&"model"));
    }
}
