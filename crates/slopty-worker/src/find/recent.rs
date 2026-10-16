//! What quick open offers before anything is typed: the files changed lately under a directory.
//!
//! First what git says changed in the worktree: modified, added, renamed, unmerged or untracked,
//! newest first, a deleted file left out. Then the files of the folders whose entries changed
//! last, newest first. Each once, files only, at most the limit asked. git's word is read each
//! time the palette opens, so it is as fresh as the disk.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

/// Folders whose files are looked at for the newest, after git's.
const RECENT_DIRS: usize = 64;
/// Files whose times are read for the newest, after git's.
const RECENT_FILES: usize = 2_048;

/// The files under `prefix` (relative to `top`, `""` or ending in `/`) changed lately, newest
/// first, at most `limit`, relative to `prefix`: git's changes in the worktree at `top`, then
/// `others`, paths relative to `top` (the files of the folders changed last), each once.
pub(super) fn recent(
    top: &Path,
    prefix: &str,
    others: impl FnOnce() -> Vec<String>,
    limit: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut take = |paths: Vec<String>, out: &mut Vec<String>| {
        for path in newest_first(top, paths) {
            if out.len() >= limit {
                return;
            }
            if let Some(rel) = path.strip_prefix(prefix)
                && !rel.is_empty()
                && seen.insert(path.clone())
            {
                out.push(rel.to_owned());
            }
        }
    };
    let changed: Vec<String> =
        git_changed(top).into_iter().filter(|path| path.starts_with(prefix)).collect();
    take(changed, &mut out);
    if out.len() < limit {
        take(others(), &mut out);
    }
    out
}

/// The paths git reports changed in the worktree at `top`, relative to it; none without git
/// or a repository.
fn git_changed(top: &Path) -> Vec<String> {
    let Some(git) = crate::changes::git() else { return Vec::new() };
    let ran = std::process::Command::new(git)
        .arg("-C")
        .arg(top)
        .args(["--no-optional-locks", "status", "--porcelain=v2", "-z", "--untracked-files=all"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(ran) = ran.inspect_err(|e| tracing::debug!(error = %e, "git status for recent files"))
    else {
        return Vec::new();
    };
    if !ran.status.success() {
        return Vec::new();
    }
    let said = String::from_utf8_lossy(&ran.stdout);
    crate::changes::status_paths(&said).into_iter().map(str::to_owned).collect()
}

/// Of `paths` (relative to `top`), the files that are there, newest first.
fn newest_first(top: &Path, paths: Vec<String>) -> Vec<String> {
    let mut dated: Vec<(SystemTime, String)> = paths
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::symlink_metadata(top.join(&path)).ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, path))
        })
        .collect();
    dated.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    dated.into_iter().map(|(_, path)| path).collect()
}

/// Of the folders under `prefix` (`dirs`, with their times as last listed), the files directly
/// in those changed last, from `files_in`; enough to find the newest among.
pub(super) fn of_newest_dirs<'a>(
    mut dirs: Vec<(&'a str, SystemTime)>,
    files_in: impl Fn(&'a str) -> Vec<String>,
) -> Vec<String> {
    dirs.sort_by_key(|&(_, time)| std::cmp::Reverse(time));
    let mut files = Vec::new();
    for (dir, _) in dirs.into_iter().take(RECENT_DIRS) {
        files.extend(files_in(dir));
        if files.len() >= RECENT_FILES {
            break;
        }
    }
    files
}

/// The files under `root` changed last, newest first, at most `limit`, relative to it: for a
/// directory no worktree holds, from `walked` (relative paths, a folder ending in `/`).
pub(super) fn walked(root: &Path, walked: Vec<String>, limit: usize) -> Vec<String> {
    let files = walked.into_iter().filter(|path| !path.ends_with('/')).collect();
    let mut out = newest_first(root, files);
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn git(top: &Path, args: &[&str]) {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(top)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Person")
            .env("GIT_AUTHOR_EMAIL", "person@example.com")
            .env("GIT_COMMITTER_NAME", "Person")
            .env("GIT_COMMITTER_EMAIL", "person@example.com")
            .output()
            .unwrap();
        assert!(ran.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ran.stderr));
    }

    /// Write `rel` under `top`, dated `age` seconds before now.
    fn write(top: &Path, rel: &str, age: u64) {
        let path = top.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{rel}, {age} s old\n")).unwrap();
        let when = SystemTime::now().checked_sub(Duration::from_secs(age)).unwrap();
        fs::File::options().write(true).open(&path).unwrap().set_modified(when).unwrap();
    }

    /// A repository with a commit of six files, dated an hour back and older.
    fn repository() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let top = fs::canonicalize(dir.path()).unwrap();
        for (n, rel) in ["src/a.rs", "src/b.rs", "src/c.rs", "docs/d.md", "e.md", "f.md"]
            .into_iter()
            .enumerate()
        {
            let age = u64::try_from(n).unwrap().saturating_mul(60).saturating_add(3_600);
            write(&top, rel, age);
        }
        git(&top, &["init", "-q", "-b", "main"]);
        git(&top, &["add", "."]);
        git(&top, &["commit", "-q", "-m", "first"]);
        (dir, top)
    }

    /// git's changes come first, newest first: an edit, a new file staged and one untracked,
    /// never a deleted one; then the other files by time; each once, files only, at most the
    /// limit, and only under the folder asked.
    #[test]
    fn the_changed_files_come_first_then_the_newest() {
        let (_dir, top) = repository();
        write(&top, "src/b.rs", 30);
        write(&top, "NOTES.md", 10);
        write(&top, "src/new.rs", 20);
        git(&top, &["add", "src/new.rs"]);
        fs::remove_file(top.join("e.md")).unwrap();
        let others = || vec!["src/a.rs".to_owned(), "src/b.rs".to_owned(), "f.md".to_owned()];
        let got = recent(&top, "", others, 10);
        assert_eq!(got, ["NOTES.md", "src/new.rs", "src/b.rs", "src/a.rs", "f.md"]);
        assert_eq!(recent(&top, "", others, 2), ["NOTES.md", "src/new.rs"], "the limit");
        assert_eq!(recent(&top, "src/", others, 10), ["new.rs", "b.rs", "a.rs"], "under src/");
    }

    /// Outside a repository there are no changes: the newest files alone.
    #[test]
    fn outside_a_repository_the_newest_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let top = fs::canonicalize(dir.path()).unwrap();
        write(&top, "old.txt", 600);
        write(&top, "new.txt", 5);
        let got = recent(&top, "", || vec!["old.txt".to_owned(), "new.txt".to_owned()], 10);
        assert_eq!(got, ["new.txt", "old.txt"]);
        let walked =
            walked(&top, vec!["sub/".to_owned(), "old.txt".to_owned(), "new.txt".to_owned()], 1);
        assert_eq!(walked, ["new.txt"]);
    }

    /// The folders changed last give their files first.
    #[test]
    fn the_newest_folders_give_their_files() {
        let now = SystemTime::now();
        let old = now.checked_sub(Duration::from_secs(600)).unwrap();
        let files =
            of_newest_dirs(vec![("old/", old), ("new/", now)], |dir| vec![format!("{dir}x")]);
        assert_eq!(files, ["new/x", "old/x"]);
    }
}
