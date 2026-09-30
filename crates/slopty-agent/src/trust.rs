//! Trusting a folder for Claude Code before an agent starts in it.
//!
//! An interactive `claude` in a folder nobody has trusted first asks whether to trust it, and
//! until someone answers it runs no hook at all, those on `--settings` included: an agent
//! Slopty starts in a folder Slopty made (a task's worktree) would sit at that dialog, silent.
//! [`trust`] marks such a folder trusted ahead of time, the way the person's own "yes" is kept.
//!
//! The mechanism, as Claude Code documents it (<https://code.claude.com/docs/en/permissions>,
//! checked 2026-09-30 against CLI 2.1.285, whose bundle does the same):
//!
//! - The answer is kept in Claude Code's global config, `~/.claude.json`
//!   (`$CLAUDE_CONFIG_DIR/.claude.json` when that is set), as
//!   `projects["<path>"].hasTrustDialogAccepted: true`. The docs name setting that key as the way
//!   to trust a folder without the dialog.
//! - Inside a repository the path is the repository's root; in a linked worktree it is the main
//!   checkout's root, so every worktree of a trusted repository is trusted ([`key`]).
//! - Outside a repository, trusting a folder covers the folders below it, but not a repository
//!   nested there (CHANGELOG 2.1.232: each repository needs its own).
//! - Trust in the home directory itself lasts one session and is never kept, so it is refused.
//! - Hooks wait for trust (<https://code.claude.com/docs/en/hooks>, "Workspace trust").
//!
//! The edit only adds: nothing in the config is removed or changed but the one flag. A config
//! that is missing, empty or does not parse as an object is refused rather than written: it is
//! not one Claude Code wrote, and replacing it would lose the person's state. The file is
//! replaced through a sibling, keeping its permissions (`slopty_platform::fs::replace`), and
//! only if nobody wrote it since it was read: Claude Code rewrites it while it runs, and a
//! change it made in between is read again rather than lost. A write that lands between that
//! check and the rename can still be lost; the window is one rename.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::hooks::Outcome;

/// The variable that moves Claude Code's configuration directory.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// The key the answer is kept under.
const ACCEPTED: &str = "hasTrustDialogAccepted";

/// Times the config is read again when it changed under an edit, before giving up.
const ATTEMPTS: u8 = 3;

/// Claude Code's global config for a person whose home is `home`.
///
/// `config_dir` is the value of [`CONFIG_DIR_ENV`] when it is set. A legacy `.config.json` in
/// the configuration directory is the one read when it exists, as Claude Code reads it.
#[must_use]
pub fn config_path(home: &Path, config_dir: Option<&Path>) -> PathBuf {
    let legacy = config_dir.map_or_else(|| home.join(".claude"), Path::to_path_buf);
    let legacy = legacy.join(".config.json");
    if legacy.is_file() {
        return legacy;
    }
    config_dir.unwrap_or(home).join(".claude.json")
}

/// [`config_path`] for this process's person: its home and its [`CONFIG_DIR_ENV`].
#[must_use]
pub fn this_config_path() -> PathBuf {
    let dir = std::env::var_os(CONFIG_DIR_ENV).filter(|dir| !dir.is_empty()).map(PathBuf::from);
    config_path(&slopty_platform::dirs::home(), dir.as_deref())
}

/// The path Claude Code keeps `folder`'s trust under: the root of the repository it is in (a
/// linked worktree's main checkout), else the folder itself, with every link resolved.
///
/// # Errors
///
/// When the folder does not exist, or its repository's files cannot be read.
pub fn key(folder: &Path) -> io::Result<PathBuf> {
    let folder = std::fs::canonicalize(folder)?;
    for dir in folder.ancestors() {
        let git = dir.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&git) else { continue };
        if meta.is_dir() {
            return Ok(dir.to_path_buf());
        }
        return linked_root(dir, &git);
    }
    Ok(folder)
}

/// The root a `.git` file at `git` in `dir` belongs to: for a linked worktree, the main
/// checkout (its git directory's `commondir` names the main one); for anything else (a
/// submodule), `dir`.
fn linked_root(dir: &Path, git: &Path) -> io::Result<PathBuf> {
    let text = std::fs::read_to_string(git)?;
    let Some(gitdir) = text.lines().find_map(|line| line.strip_prefix("gitdir:")) else {
        return Ok(dir.to_path_buf());
    };
    let gitdir = dir.join(gitdir.trim());
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => gitdir.join(common.trim()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(dir.to_path_buf()),
        Err(e) => return Err(e),
    };
    let common = std::fs::canonicalize(common)?;
    match common.parent() {
        Some(root) if common.file_name().is_some_and(|name| name == ".git") => {
            Ok(root.to_path_buf())
        }
        _ => Ok(dir.to_path_buf()),
    }
}

/// Mark `folder` trusted in the config at `config` ([`config_path`]), for the person whose home
/// is `home`: [`Outcome::Unchanged`] when it was already.
///
/// # Errors
///
/// - `NotFound` when there is no config yet (Claude Code has not run for this person) or no such
///   folder;
/// - `InvalidInput` for the home directory, whose trust Claude Code never keeps;
/// - `InvalidData` for a config that is empty, does not parse, is not an object or keeps its
///   projects in something other than an object;
/// - `Interrupted` when the config changed under every attempt;
/// - whatever reading or replacing the file fails with.
pub fn trust(config: &Path, home: &Path, folder: &Path) -> io::Result<Outcome> {
    let key = key(folder)?;
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if key == home {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Claude Code never keeps trust in the home directory",
        ));
    }
    let key = key.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the folder's path is not UTF-8")
    })?;
    for _ in 0..ATTEMPTS {
        let read = std::fs::read(config)?;
        let mut doc = parse(config, &read)?;
        if !accept(&mut doc, key).map_err(|why| invalid(config, why))? {
            return Ok(Outcome::Unchanged);
        }
        let mut text = serde_json::to_vec_pretty(&doc).map_err(io::Error::other)?;
        text.push(b'\n');
        if std::fs::read(config)? != read {
            continue;
        }
        slopty_platform::fs::replace(config, &text)?;
        return Ok(Outcome::Changed);
    }
    Err(io::Error::new(
        io::ErrorKind::Interrupted,
        format!("{} kept changing while trust was added", config.display()),
    ))
}

/// The config's document, when it is one Claude Code wrote: a JSON object.
fn parse(config: &Path, bytes: &[u8]) -> io::Result<Value> {
    if bytes.trim_ascii().is_empty() {
        return Err(invalid(config, "is empty"));
    }
    let doc: Value = serde_json::from_slice(bytes).map_err(|e| invalid(config, &e.to_string()))?;
    if !doc.is_object() {
        return Err(invalid(config, "is not a JSON object"));
    }
    Ok(doc)
}

fn invalid(config: &Path, why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{} {why}; left alone", config.display()))
}

/// Set the flag for `key`, adding `projects` and its entry when missing. Whether it changed;
/// an error when something already there has another shape, which is left as it is.
fn accept(doc: &mut Value, key: &str) -> Result<bool, &'static str> {
    let root = doc.as_object_mut().ok_or("is not a JSON object")?;
    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("keeps its projects in something other than an object")?;
    let project = projects
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("keeps this project in something other than an object")?;
    if project.get(ACCEPTED) == Some(&Value::Bool(true)) {
        return Ok(false);
    }
    project.insert(ACCEPTED.to_owned(), json!(true));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
        home: PathBuf,
        config: PathBuf,
    }

    /// A temporary home with a config Claude Code could have written.
    fn home_with(config: &str) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = std::fs::canonicalize(dir.path()).expect("canonical").join("home");
        std::fs::create_dir_all(&home).expect("home");
        let path = config_path(&home, None);
        std::fs::write(&path, config).expect("config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        Fixture { dir, home, config: path }
    }

    fn doc(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).expect("read")).expect("json")
    }

    /// The flag is added beside everything the config held, which stays as it was; a second
    /// time changes nothing, and the file keeps its permissions and leaves no sibling behind.
    #[test]
    fn a_folder_is_trusted_by_adding_one_flag() {
        let h = home_with(
            r#"{"numStartups":7,"oauthAccount":{"emailAddress":"x"},
                "projects":{"/elsewhere":{"hasTrustDialogAccepted":false,"allowedTools":["Bash"]}}}"#,
        );
        let folder = h.home.join("work/task-1");
        std::fs::create_dir_all(&folder).expect("folder");
        let before = doc(&h.config);
        assert_eq!(trust(&h.config, &h.home, &folder).expect("trust"), Outcome::Changed);
        let after = doc(&h.config);
        let key = folder.to_str().expect("utf-8");
        assert_eq!(after["projects"][key], json!({ "hasTrustDialogAccepted": true }));
        let mut back = after;
        back["projects"].as_object_mut().expect("projects").remove(key);
        assert_eq!(back, before, "nothing else moved");
        assert_eq!(trust(&h.config, &h.home, &folder).expect("again"), Outcome::Unchanged);
        let mode = std::fs::metadata(&h.config).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "its permissions are kept");
        let left = std::fs::read_dir(&h.home).expect("home").count();
        assert_eq!(left, 2, "the config and the work folder, no temporary file");
    }

    /// An entry the person already has keeps its other fields; a project named through a link
    /// is kept under its resolved path.
    #[test]
    fn an_existing_entry_gains_the_flag_under_the_resolved_path() {
        let h = home_with("{}");
        let real = h.dir.path().join("real");
        std::fs::create_dir_all(&real).expect("real");
        let real = std::fs::canonicalize(real).expect("canonical");
        let key = real.to_str().expect("utf-8").to_owned();
        std::fs::write(
            &h.config,
            json!({ "projects": { &key: { "allowedTools": ["Read"] } } }).to_string(),
        )
        .expect("config");
        let link = h.home.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        assert_eq!(trust(&h.config, &h.home, &link).expect("trust"), Outcome::Changed);
        assert_eq!(
            doc(&h.config)["projects"][&key],
            json!({ "allowedTools": ["Read"], "hasTrustDialogAccepted": true })
        );
    }

    /// A config that is missing, empty, not JSON, not an object, or shaped otherwise where the
    /// flag goes is left exactly as it was; so is the home directory's trust.
    #[test]
    fn a_config_that_is_not_claude_codes_is_left_alone() {
        let folder = |h: &Fixture| {
            let folder = h.home.join("w");
            std::fs::create_dir_all(&folder).expect("folder");
            folder
        };
        for (text, kind) in [
            ("", io::ErrorKind::InvalidData),
            ("  \n", io::ErrorKind::InvalidData),
            ("{\"projects\":", io::ErrorKind::InvalidData),
            ("[1]", io::ErrorKind::InvalidData),
            (r#"{"projects":[]}"#, io::ErrorKind::InvalidData),
        ] {
            let h = home_with(text);
            let err = trust(&h.config, &h.home, &folder(&h)).expect_err(text);
            assert_eq!(err.kind(), kind, "{text:?}: {err}");
            assert_eq!(std::fs::read_to_string(&h.config).expect("read"), text, "untouched");
        }
        let h = home_with("{}");
        let err = trust(&h.config, &h.home, &h.home).expect_err("home");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read_to_string(&h.config).expect("read"), "{}");
        std::fs::remove_file(&h.config).expect("remove");
        let err = trust(&h.config, &h.home, &folder(&h)).expect_err("no config");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!h.config.exists(), "none is made");
    }

    /// Inside a repository the key is its root; in a linked worktree, the main checkout's, so
    /// one trusted repository covers all its worktrees; a submodule is its own.
    #[test]
    fn a_worktree_is_trusted_through_its_main_checkout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        let main = base.join("mirror");
        std::fs::create_dir_all(main.join(".git/worktrees/task-1")).expect("git dir");
        std::fs::create_dir_all(main.join("src/deep")).expect("src");
        assert_eq!(key(&main.join("src/deep")).expect("key"), main);

        let tree = base.join("trees/task-1");
        std::fs::create_dir_all(tree.join("src")).expect("tree");
        let gitdir = main.join(".git/worktrees/task-1");
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", gitdir.display()))
            .expect(".git file");
        std::fs::write(gitdir.join("commondir"), "../..\n").expect("commondir");
        assert_eq!(key(&tree.join("src")).expect("key"), main);

        let module = main.join("vendor/lib");
        std::fs::create_dir_all(&module).expect("module");
        std::fs::create_dir_all(main.join(".git/modules/lib")).expect("module git dir");
        std::fs::write(module.join(".git"), "gitdir: ../../.git/modules/lib\n").expect(".git");
        assert_eq!(key(&module).expect("key"), module);

        let plain = base.join("plain/folder");
        std::fs::create_dir_all(&plain).expect("plain");
        assert_eq!(key(&plain).expect("key"), plain);
    }

    /// The config moves with `CLAUDE_CONFIG_DIR`, and a legacy `.config.json` there is the one
    /// read.
    #[test]
    fn the_config_is_found_where_claude_code_reads_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let moved = dir.path().join("moved");
        std::fs::create_dir_all(&moved).expect("moved");
        assert_eq!(config_path(&home, None), home.join(".claude.json"));
        assert_eq!(config_path(&home, Some(&moved)), moved.join(".claude.json"));
        std::fs::write(moved.join(".config.json"), "{}").expect("legacy");
        assert_eq!(config_path(&home, Some(&moved)), moved.join(".config.json"));
    }
}
