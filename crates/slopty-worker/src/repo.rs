//! Which repository a working directory is in, and what it has checked out.
//!
//! The workspace names shells by repository, and the only machine
//! that can answer "which repository is this" is the one the shell runs on. This is that
//! answer: a walk up the directory tree for a `.git` entry, no `git` subprocess and no libgit,
//! so it costs a handful of `stat` calls per `cd` and cannot hang on a lock or an index. The
//! branch is the same kind of answer: `HEAD` read as a file, never `git branch`.

use std::path::{Path, PathBuf};

/// The repository root containing `cwd`, if any.
///
/// The root is the nearest ancestor — starting at `cwd` itself — holding a `.git` entry. That
/// entry is a **directory** in an ordinary checkout and a **file** in a worktree or a
/// submodule; the `gitdir:` link inside such a file is deliberately not followed, so a worktree
/// is its own repository and not the checkout it was made from. That is what the client wants:
/// two worktrees of one project are two places to work, not one.
///
/// A repository nested inside another wins over the outer one, because the walk stops at the
/// first entry it meets. `None` when nothing above `cwd` has one, or when `cwd` cannot be
/// resolved — a directory that has since been removed or renamed answers nothing rather than
/// guessing from the stale string.
#[must_use]
pub fn root_of(cwd: &Path) -> Option<PathBuf> {
    // Through symlinks first: the shell reports the path it walked in through, and two shells
    // that reached the same checkout by different names must land in the same block.
    let start = std::fs::canonicalize(cwd).ok()?;
    let mut dir = start.as_path();
    loop {
        // `symlink_metadata`, not `exists`: a `.git` that is a dangling symlink still marks a
        // checkout, and this way a broken link is not silently skipped for its parent's sake.
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// [`root_of`] on a path that arrived as a string (OSC 7 gives us one), as a string.
#[must_use]
pub fn root_of_str(cwd: &str) -> Option<String> {
    root_of(Path::new(cwd)).map(|root| root.to_string_lossy().into_owned())
}

/// The `HEAD` file of the repository rooted at `root` (a [`root_of`] answer).
///
/// In an ordinary checkout that is `.git/HEAD`. In a worktree or a submodule `.git` is a file
/// whose `gitdir:` line names the git directory, relative to `root` or absolute, and `HEAD` is
/// in there: each worktree has its own.
#[must_use]
pub fn head_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if std::fs::metadata(&dot_git).ok()?.is_dir() {
        return Some(dot_git.join("HEAD"));
    }
    let link = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = link.lines().next()?.strip_prefix("gitdir:")?.trim();
    (!gitdir.is_empty()).then(|| root.join(gitdir).join("HEAD"))
}

/// [`branch_at`] of [`head_of`]: what the repository rooted at `root` has checked out.
#[must_use]
pub fn branch_of(root: &Path) -> Option<String> {
    branch_at(&head_of(root)?)
}

/// How many hex digits of a detached `HEAD`'s commit name it, as `git`'s default abbreviation.
const SHORT_HASH: usize = 7;

/// What the `HEAD` file at `head` has checked out.
///
/// The branch name (`refs/heads/` dropped), any other ref as written below `refs/`, or the
/// commit abbreviated when `HEAD` is detached. `None` when the file cannot be read or holds
/// neither.
#[must_use]
pub fn branch_at(head: &Path) -> Option<String> {
    let text = std::fs::read_to_string(head).ok()?;
    let line = text.lines().next()?.trim();
    if let Some(target) = line.strip_prefix("ref:") {
        let target = target.trim();
        let name = target
            .strip_prefix("refs/heads/")
            .or_else(|| target.strip_prefix("refs/"))
            .unwrap_or(target);
        return (!name.is_empty()).then(|| name.to_owned());
    }
    let hash = line.get(..SHORT_HASH)?;
    (line.len() >= 40 && line.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// `TempDir` paths go through `/var` → `/private/var` on macOS; compare canonical to
    /// canonical or every assertion here is about symlinks instead of repositories.
    fn real(path: &Path) -> PathBuf {
        fs::canonicalize(path).expect("the temp tree exists")
    }

    #[test]
    fn a_checkout_and_everything_under_it() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        fs::create_dir_all(repo.join(".git")).expect("mkdir");
        fs::create_dir_all(repo.join("crates/a/src")).expect("mkdir");

        assert_eq!(root_of(&repo), Some(real(&repo)));
        assert_eq!(root_of(&repo.join("crates/a/src")), Some(real(&repo)));
    }

    #[test]
    fn a_worktree_is_its_own_repository() {
        let tmp = tempfile::tempdir().expect("temp");
        let main = tmp.path().join("project");
        let tree = tmp.path().join("project-wt/feature");
        fs::create_dir_all(main.join(".git/worktrees/feature")).expect("mkdir");
        fs::create_dir_all(tree.join("src")).expect("mkdir");
        // What git writes in a worktree: a file, not a directory.
        fs::write(tree.join(".git"), "gitdir: ../../project/.git/worktrees/feature\n")
            .expect("write");

        assert_eq!(root_of(&tree.join("src")), Some(real(&tree)));
        assert_ne!(root_of(&tree.join("src")), root_of(&main), "not the checkout it came from");
    }

    #[test]
    fn the_innermost_repository_wins() {
        let tmp = tempfile::tempdir().expect("temp");
        let outer = tmp.path().join("outer");
        let inner = outer.join("vendor/inner");
        fs::create_dir_all(outer.join(".git")).expect("mkdir");
        fs::create_dir_all(inner.join(".git")).expect("mkdir");
        fs::create_dir_all(inner.join("src")).expect("mkdir");

        assert_eq!(root_of(&inner.join("src")), Some(real(&inner)));
        assert_eq!(root_of(&outer.join("src2")), None, "a path that does not exist");
        assert_eq!(root_of(&outer), Some(real(&outer)));
    }

    #[test]
    fn no_repository_and_no_directory() {
        let tmp = tempfile::tempdir().expect("temp");
        let plain = tmp.path().join("just/a/tree");
        fs::create_dir_all(&plain).expect("mkdir");

        // A temp directory is not in a repository — unless the machine's temp lives in one,
        // which would make every assertion here meaningless, so say so instead of failing oddly.
        assert_eq!(root_of(&plain), root_of(tmp.path()), "the tree above decides");

        let gone = plain.join("removed");
        assert_eq!(root_of(&gone), None, "a directory that is not there answers nothing");
    }

    /// A checkout at `dir` with `head` in its `.git/HEAD`.
    fn checkout(dir: &Path, head: &str) {
        fs::create_dir_all(dir.join(".git")).expect("mkdir");
        fs::write(dir.join(".git/HEAD"), head).expect("write");
    }

    fn branch_in(dir: &Path) -> Option<String> {
        branch_of(&root_of(dir)?)
    }

    #[test]
    fn the_branch_a_checkout_has_out() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        checkout(&repo, "ref: refs/heads/feature/rows\n");
        fs::create_dir_all(repo.join("src")).expect("mkdir");

        assert_eq!(branch_in(&repo.join("src")).as_deref(), Some("feature/rows"));
        assert_eq!(head_of(&real(&repo)), Some(real(&repo).join(".git/HEAD")));
    }

    #[test]
    fn a_worktree_reads_its_own_head_through_the_gitdir_link() {
        let tmp = tempfile::tempdir().expect("temp");
        let main = tmp.path().join("project");
        checkout(&main, "ref: refs/heads/main\n");
        let gitdir = main.join(".git/worktrees/feature");
        fs::create_dir_all(&gitdir).expect("mkdir");
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature\n").expect("write");
        let relative = tmp.path().join("project-wt/feature");
        fs::create_dir_all(&relative).expect("mkdir");
        fs::write(relative.join(".git"), "gitdir: ../../project/.git/worktrees/feature\n")
            .expect("write");
        let absolute = tmp.path().join("elsewhere");
        fs::create_dir_all(&absolute).expect("mkdir");
        fs::write(absolute.join(".git"), format!("gitdir: {}\n", gitdir.display())).expect("write");

        assert_eq!(branch_in(&relative).as_deref(), Some("feature"));
        assert_eq!(branch_in(&absolute).as_deref(), Some("feature"));
        assert_eq!(branch_in(&main).as_deref(), Some("main"), "the main checkout keeps its own");
    }

    #[test]
    fn a_detached_head_is_its_short_hash() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        checkout(&repo, "4f1c2e9a0b7d3c5e8f6a1b2c3d4e5f60718293a4\n");
        assert_eq!(branch_in(&repo).as_deref(), Some("4f1c2e9"));

        // A SHA-256 repository's hashes are longer; the abbreviation is the same.
        checkout(&repo, &format!("{}\n", "ab".repeat(32)));
        assert_eq!(branch_in(&repo).as_deref(), Some("abababa"));
    }

    #[test]
    fn no_branch_without_a_readable_head() {
        let tmp = tempfile::tempdir().expect("temp");
        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).expect("mkdir");
        let bare = tmp.path().join("bare");
        fs::create_dir_all(bare.join(".git")).expect("mkdir");
        let garbled = tmp.path().join("garbled");
        checkout(&garbled, "not a head\n");
        let dangling = tmp.path().join("dangling");
        fs::create_dir_all(&dangling).expect("mkdir");
        fs::write(dangling.join(".git"), "gitdir: nowhere\n").expect("write");

        assert_eq!(head_of(&plain), None, "no .git at all");
        assert_eq!(branch_in(&bare), None, ".git with no HEAD in it");
        assert_eq!(branch_in(&garbled), None, "a HEAD that names nothing");
        assert_eq!(branch_in(&dangling), None, "a gitdir link to nothing");
    }

    /// What the session actor pays when a command ends: the repository found again from the
    /// directory (the walk up and the canonicalisation) and its `HEAD` read. Run with
    /// `cargo nextest run -p slopty-worker --release --run-ignored only place_cost --no-capture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn place_cost() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        checkout(&repo, "ref: refs/heads/main\n");
        let deep = repo.join("crates/slopty-worker/src/orchestrate");
        fs::create_dir_all(&deep).expect("mkdir");
        let cwd = deep.to_str().expect("utf-8 temp path");
        let root = real(&repo);
        let reads = 20_000_u32;

        let started = std::time::Instant::now();
        for _ in 0..reads {
            let found = root_of_str(cwd);
            std::hint::black_box(found.as_deref().map(Path::new).and_then(branch_of));
        }
        let whole = started.elapsed() / reads;
        let started = std::time::Instant::now();
        for _ in 0..reads {
            std::hint::black_box(root_of_str(cwd));
        }
        let walk = started.elapsed() / reads;
        let started = std::time::Instant::now();
        for _ in 0..reads {
            std::hint::black_box(branch_of(&root));
        }
        let head = started.elapsed() / reads;
        eprintln!(
            "place_cost: {} ns for root and branch from a directory four deep, {} ns for the root alone, {} ns for the branch alone",
            whole.as_nanos(),
            walk.as_nanos(),
            head.as_nanos()
        );
    }

    #[test]
    fn the_string_form_agrees_with_the_path_form() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        fs::create_dir_all(repo.join(".git")).expect("mkdir");
        let as_str = repo.to_str().expect("utf-8 temp path");

        assert_eq!(root_of_str(as_str), Some(real(&repo).to_string_lossy().into_owned()));
        assert_eq!(root_of_str("/definitely/not/here"), None);
    }
}
