//! A repository's own setup, run in a new worktree before anything starts there
//! (`docs/decisions/projects.md`, "A new worktree runs the repository's setup").
//!
//! No setup file is shared between tools (`.research/worktree-setup-2026-10-06.md`), so the
//! one a repository already keeps for another tool is read, from the new worktree's own
//! checkout: the first, in [`SOURCES`]' order, whose setup is not empty ([`find`]). Sources are
//! never merged. It runs once ([`run`]), after the ignored files `.worktreeinclude` names are
//! copied in: through the person's login shell for their `PATH` and toolchains, then `bash -e`,
//! the shell those tools' scripts are written for, stopping at the first command that fails.
//! Its stdin is closed, it runs in the worktree, and what it prints is said as it goes. It is
//! given the places in the environment under Slopty's names, Conductor's (which other tools
//! copy) and its own tool's.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slopty_proto::thread::wire::Setup;
use tokio::io::{AsyncBufReadExt as _, BufReader};

/// The files a setup is read from, in the order they are tried.
pub const SOURCES: [&str; 7] = [
    ".conductor/settings.toml",
    "conductor.json",
    ".codex/environments/environment.toml",
    ".cursor/worktrees.json",
    ".superset/config.json",
    ".superset/setup.sh",
    "t3.json",
];

/// A setup found in a worktree ([`find`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Found {
    /// The file it was read from, one of [`SOURCES`].
    pub from: &'static str,
    /// The shell script it runs.
    pub script: String,
}

/// Where a setup runs, and for what.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Places<'a> {
    /// The clone the worktree was made from.
    pub root: &'a Path,
    /// The worktree.
    pub tree: &'a Path,
    /// The worktree's name.
    pub name: &'a str,
    /// The branch it started from, if one.
    pub base: Option<&'a str>,
}

/// How a setup that did not succeed ended: what it said last, and its exit status (`None`
/// when a signal ended it, or it could not start).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Failed {
    /// What ran, and its last lines.
    pub setup: Setup,
    /// Its exit status.
    pub code: Option<i32>,
}

/// How often a running setup's last lines are said, at most.
const SAID_EVERY: Duration = Duration::from_millis(250);

/// How much of one line of output is kept.
const LINE_MAX: usize = 400;

/// The setup the worktree at `tree` keeps for another tool: the first of [`SOURCES`] there
/// whose setup is not empty. A file that does not parse is passed over, as one with none.
#[must_use]
pub fn find(tree: &Path) -> Option<Found> {
    SOURCES.into_iter().find_map(|from| {
        let path = tree.join(from);
        let text = std::fs::read_to_string(&path).ok();
        let script = match from {
            ".superset/setup.sh" => path.is_file().then(|| format!("bash {}", quoted(&path))),
            _ => text.as_deref().and_then(|text| read(from, text, tree)),
        };
        let script = script?.trim().to_owned();
        (!script.is_empty()).then_some(Found { from, script })
    })
}

/// The script `text`, read as `from`, names.
fn read(from: &str, text: &str, tree: &Path) -> Option<String> {
    match from {
        // `[scripts] setup = "…"`.
        ".conductor/settings.toml" => {
            let doc: toml::Table = toml::from_str(text).ok()?;
            Some(doc.get("scripts")?.get("setup")?.as_str()?.to_owned())
        }
        // `{"scripts": {"setup": "…"}}`.
        "conductor.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            Some(doc.get("scripts")?.get("setup")?.as_str()?.to_owned())
        }
        // `[setup] script = "…"`.
        ".codex/environments/environment.toml" => {
            let doc: toml::Table = toml::from_str(text).ok()?;
            Some(doc.get("setup")?.get("script")?.as_str()?.to_owned())
        }
        // `setup-worktree-unix`, else `setup-worktree`: commands, or the path of a script
        // relative to `.cursor/`.
        ".cursor/worktrees.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            let setup = ["setup-worktree-unix", "setup-worktree"]
                .into_iter()
                .find_map(|key| doc.get(key).filter(|v| !is_empty(v)))?;
            match setup.as_str() {
                Some(script) => {
                    Some(format!("bash {}", quoted(&tree.join(".cursor").join(script))))
                }
                None => joined(setup),
            }
        }
        // `{"setup": ["…", …]}`.
        ".superset/config.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            joined(doc.get("setup")?)
        }
        // The scripts marked `runOnWorktreeCreate`, in order.
        "t3.json" => {
            let doc: serde_json::Value = serde_json::from_str(text).ok()?;
            let commands: Vec<&str> = doc
                .get("scripts")?
                .as_array()?
                .iter()
                .filter(|s| {
                    s.get("runOnWorktreeCreate").and_then(serde_json::Value::as_bool) == Some(true)
                })
                .filter_map(|s| s.get("command")?.as_str())
                .filter(|c| !c.trim().is_empty())
                .collect();
            Some(commands.join(" && "))
        }
        _ => None,
    }
}

/// Whether a setup's value says nothing: an empty string or list.
fn is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => s.trim().is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// A list of commands as one script that stops at the first to fail.
pub(super) fn joined(commands: &serde_json::Value) -> Option<String> {
    let commands: Vec<&str> = commands
        .as_array()?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|c| !c.trim().is_empty())
        .collect();
    Some(commands.join(" && "))
}

/// `path` quoted for a shell.
pub(super) fn quoted(path: &Path) -> String {
    slopty_core::shell_quote(&path.to_string_lossy())
}

/// The environment a setup from `from` runs with in `at`: Slopty's names, Conductor's, and its
/// own tool's.
#[must_use]
pub fn env(from: &str, at: &Places<'_>) -> Vec<(String, String)> {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    let (root, tree, name) = (text(at.root), text(at.tree), at.name.to_owned());
    let base = at.base.unwrap_or_default().to_owned();
    let mut env = Vec::new();
    for prefix in ["SLOPTY", "CONDUCTOR"] {
        env.push((format!("{prefix}_ROOT_PATH"), root.clone()));
        env.push((format!("{prefix}_WORKSPACE_PATH"), tree.clone()));
        env.push((format!("{prefix}_WORKSPACE_NAME"), name.clone()));
        env.push((format!("{prefix}_DEFAULT_BRANCH"), base.clone()));
    }
    match from {
        ".cursor/worktrees.json" => env.push(("ROOT_WORKTREE_PATH".to_owned(), root)),
        ".superset/config.json" | ".superset/setup.sh" => {
            env.push(("SUPERSET_ROOT_PATH".to_owned(), root));
            env.push(("SUPERSET_WORKSPACE_PATH".to_owned(), tree));
            env.push(("SUPERSET_WORKSPACE_NAME".to_owned(), name));
        }
        "t3.json" => {
            env.push(("T3CODE_PROJECT_ROOT".to_owned(), root));
            env.push(("T3CODE_WORKTREE_PATH".to_owned(), tree));
        }
        _ => {}
    }
    env
}

/// The command line a setup runs as, with `shell` the person's login shell: the shell reads
/// the person's profile, then hands the script, from the environment, to `bash -e`.
#[must_use]
pub fn command_line(shell: &str) -> Vec<String> {
    [shell, "-l", "-i", "-c", "exec bash -e -c \"$SLOPTY_SETUP\""].map(str::to_owned).to_vec()
}

/// Run `found` in `at` and wait for it, saying its last lines to `said` as they come, a few
/// times a second at most.
///
/// It runs in a process group of its own, killed whole when the caller stops waiting.
///
/// # Errors
/// [`Failed`] when it could not start, exited with a failure, or a signal ended it.
pub async fn run(
    found: &Found,
    at: &Places<'_>,
    said: &(dyn Fn(&Setup) + Sync),
) -> Result<(), Failed> {
    let mut setup = Setup { from: found.from.to_owned(), tail: Vec::new() };
    let mut command = tokio::process::Command::new(super::script::login_shell());
    let line = command_line(&super::script::login_shell());
    command
        .args(line.get(1..).unwrap_or_default())
        .current_dir(at.tree)
        .env("SLOPTY_SETUP", &found.script)
        .envs(env(found.from, at))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let failed = |setup: Setup, code| Failed { setup, code };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            setup.tail = vec![format!("could not start the setup: {e}")];
            return Err(failed(setup, None));
        }
    };
    // Declared after the child, so it drops first: the group is killed while its leader is
    // unreaped, and so still this child's.
    let mut group = crate::facts::Group::of(&child);
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(failed(setup, None));
    };
    let (mut out, mut err) =
        (BufReader::new(stdout).split(b'\n'), BufReader::new(stderr).split(b'\n'));
    let mut tail = VecDeque::with_capacity(Setup::TAIL);
    let mut tick = tokio::time::interval(SAID_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (mut out_open, mut err_open, mut changed) = (true, true, false);
    while out_open || err_open {
        tokio::select! {
            line = out.next_segment(), if out_open => match line {
                Ok(Some(line)) => changed |= keep(&mut tail, &line),
                _ => out_open = false,
            },
            line = err.next_segment(), if err_open => match line {
                Ok(Some(line)) => changed |= keep(&mut tail, &line),
                _ => err_open = false,
            },
            _ = tick.tick() => if changed {
                setup.tail = tail.iter().cloned().collect();
                said(&setup);
                changed = false;
            },
        }
    }
    let status = child.wait().await;
    group.reaped();
    setup.tail = tail.into_iter().collect();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(failed(setup, status.code())),
        Err(e) => {
            setup.tail.push(format!("the setup was lost: {e}"));
            Err(failed(setup, None))
        }
    }
}

/// Keep `line` of output as the newest of `tail`: what a carriage return last drew of it,
/// without its terminal escapes, cut to [`LINE_MAX`]. Returns whether it was kept: a blank
/// line is not.
fn keep(tail: &mut VecDeque<String>, line: &[u8]) -> bool {
    let line = String::from_utf8_lossy(line);
    let drawn = line.trim_end_matches(['\r', '\n']).rsplit('\r').next().unwrap_or_default();
    let plain = plain(drawn);
    let plain = plain.trim_end();
    if plain.trim().is_empty() {
        return false;
    }
    let cut = plain.char_indices().nth(LINE_MAX).map_or(plain.len(), |(at, _)| at);
    if tail.len() == Setup::TAIL {
        tail.pop_front();
    }
    tail.push_back(plain.get(..cut).unwrap_or(plain).to_owned());
    true
}

/// `text` without its terminal escapes: control sequences (`ESC [ … final`), operating system
/// commands (`ESC ] … BEL` or `ESC \`) and other two-byte escapes.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() || c == '\t' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => while chars.next().is_some_and(|c| !('@'..='~').contains(&c)) {},
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The marker a new worktree's git directory holds while its setup has not yet succeeded or
/// been passed over.
const PENDING: &str = "slopty-setup-pending";

/// Mark the new worktree at `tree` as waiting for its setup.
pub(crate) fn mark_pending(tree: &Path) {
    if let Some(dir) = super::git_dir(tree)
        && let Err(e) = std::fs::write(dir.join(PENDING), b"")
    {
        tracing::warn!("a new worktree's setup could not be marked: {e}");
    }
}

/// Whether the worktree at `tree` waits for its setup: made by Slopty, and its setup has not
/// yet succeeded or been passed over.
#[must_use]
pub(crate) fn pending(tree: &Path) -> bool {
    super::git_dir(tree).is_some_and(|dir| dir.join(PENDING).is_file())
}

/// The worktree at `tree` waits for its setup no more.
pub(crate) fn settled(tree: &Path) {
    if let Some(dir) = super::git_dir(tree) {
        let _gone = std::fs::remove_file(dir.join(PENDING));
    }
}

/// The lock a worktree's setup is run under, so a start sent again while it runs (after a
/// reconnect) waits for it rather than running it twice.
pub(crate) fn lock(tree: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::LazyLock<
        parking_lot::Mutex<
            std::collections::HashMap<PathBuf, std::sync::Weak<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::LazyLock::new(Default::default);
    let mut locks = LOCKS.lock();
    locks.retain(|_, held| held.strong_count() > 0);
    if let Some(held) = locks.get(tree).and_then(std::sync::Weak::upgrade) {
        return held;
    }
    let held = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(tree.to_path_buf(), std::sync::Arc::downgrade(&held));
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("dir");
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(path, text).expect("write");
        }
        dir
    }

    /// Each tool's file is read in its own form, and the first with a setup that is not empty
    /// wins: an empty Codex script (as the app writes by default) gives way to Cursor's.
    #[test]
    fn the_first_setup_that_says_something_is_read() {
        let found = |files: &[(&str, &str)]| {
            let tree = tree_with(files);
            find(tree.path())
                .map(|f| (f.from, f.script.replace(&*tree.path().to_string_lossy(), "<tree>")))
        };
        assert_eq!(
            found(&[(".conductor/settings.toml", "[scripts]\nsetup = \"bun install\"\n")]),
            Some((".conductor/settings.toml", "bun install".to_owned()))
        );
        assert_eq!(
            found(&[("conductor.json", r#"{"scripts":{"setup":"npm ci","run":"npm run dev"}}"#)]),
            Some(("conductor.json", "npm ci".to_owned()))
        );
        assert_eq!(
            found(&[
                (".codex/environments/environment.toml", "[setup]\nscript = \"\"\n"),
                (
                    ".cursor/worktrees.json",
                    r#"{"setup-worktree":["npm ci","cp $ROOT_WORKTREE_PATH/.env .env"]}"#
                ),
            ]),
            Some((
                ".cursor/worktrees.json",
                "npm ci && cp $ROOT_WORKTREE_PATH/.env .env".to_owned()
            ))
        );
        assert_eq!(
            found(&[(
                ".cursor/worktrees.json",
                r#"{"setup-worktree":"setup.sh","setup-worktree-unix":"unix.sh"}"#
            )]),
            Some((".cursor/worktrees.json", "bash <tree>/.cursor/unix.sh".to_owned()))
        );
        assert_eq!(
            found(&[
                (".superset/config.json", r#"{"setup":[]}"#),
                (".superset/setup.sh", "make\n")
            ]),
            Some((".superset/setup.sh", "bash <tree>/.superset/setup.sh".to_owned()))
        );
        let t3 = r#"{"scripts":[{"id":"dev","command":"bun dev"},{"id":"i","command":"bun i","runOnWorktreeCreate":true}]}"#;
        assert_eq!(found(&[("t3.json", t3)]), Some(("t3.json", "bun i".to_owned())));
        assert_eq!(
            found(&[("conductor.json", "{not json"), ("t3.json", r#"{"scripts":[]}"#)]),
            None,
            "a file that does not parse is passed over, and nothing found is no setup"
        );
    }

    /// Slopty's names and Conductor's always, then the tool's own.
    #[test]
    fn a_setup_is_told_its_places_under_its_tools_names() {
        let at = Places {
            root: Path::new("/r"),
            tree: Path::new("/r/.claude/worktrees/w"),
            name: "w",
            base: Some("main"),
        };
        let env = env(".superset/config.json", &at);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SLOPTY_WORKSPACE_PATH"), Some("/r/.claude/worktrees/w"));
        assert_eq!(get("CONDUCTOR_ROOT_PATH"), Some("/r"));
        assert_eq!(get("CONDUCTOR_DEFAULT_BRANCH"), Some("main"));
        assert_eq!(get("SUPERSET_WORKSPACE_NAME"), Some("w"));
        assert_eq!(get("T3CODE_WORKTREE_PATH"), None, "not another tool's");
    }

    /// A line is what its last carriage return drew, without escapes; blank lines are not
    /// kept, and the tail keeps only the newest.
    #[test]
    fn output_is_kept_as_it_last_read() {
        let mut tail = VecDeque::new();
        assert!(keep(&mut tail, b"\x1b[32mResolving\x1b[0m 1/3\r\x1b]8;;u\x07Resolving 3/3\r\n"));
        assert!(!keep(&mut tail, b"   \r"));
        assert_eq!(tail, ["Resolving 3/3"]);
        for n in 0..Setup::TAIL {
            keep(&mut tail, format!("line {n}").as_bytes());
        }
        assert_eq!(tail.len(), Setup::TAIL);
        assert_eq!(tail.front().map(String::as_str), Some("line 0"));
    }

    /// The script runs with `bash -e` in the worktree with its places, stopping at the first
    /// failure, and what it printed on either stream is in its failure, the newest last.
    #[tokio::test]
    async fn a_setup_runs_in_the_worktree_and_stops_at_its_first_failure() {
        let tree = tempfile::tempdir().expect("dir");
        let at = Places { root: Path::new("/r"), tree: tree.path(), name: "w", base: None };
        let ok = Found {
            from: "conductor.json",
            script: "echo \"$CONDUCTOR_WORKSPACE_NAME\" > made\npwd -P >> made".to_owned(),
        };
        run(&ok, &at, &|_| {}).await.expect("it succeeds");
        let made = std::fs::read_to_string(tree.path().join("made")).expect("made");
        let real = std::fs::canonicalize(tree.path()).expect("real");
        assert_eq!(made, format!("w\n{}\n", real.display()));

        let failing = Found {
            from: "conductor.json",
            script: "echo out; echo err >&2; false\necho never".to_owned(),
        };
        let failed = run(&failing, &at, &|_| {}).await.expect_err("it fails");
        assert_eq!(failed.code, Some(1));
        assert_eq!(failed.setup.from, "conductor.json");
        assert!(failed.setup.tail.contains(&"out".to_owned()), "{:?}", failed.setup.tail);
        assert!(failed.setup.tail.contains(&"err".to_owned()), "{:?}", failed.setup.tail);
        assert!(!failed.setup.tail.contains(&"never".to_owned()), "stopped at `false`");
    }
}
