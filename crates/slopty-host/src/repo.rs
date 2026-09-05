//! Which repository a working directory is in.
//!
//! The canvas groups shells by repository (`slopty_client::arrange`), and the only machine
//! that can answer "which repository is this" is the one the shell runs on. This is that
//! answer: a walk up the directory tree for a `.git` entry, no `git` subprocess and no libgit,
//! so it costs a handful of `stat` calls per `cd` and cannot hang on a lock or an index.

use std::path::{Path, PathBuf};

/// The repository root containing `cwd`, if any.
///
/// The root is the nearest ancestor — starting at `cwd` itself — holding a `.git` entry. That
/// entry is a **directory** in an ordinary checkout and a **file** in a worktree or a
/// submodule; the `gitdir:` link inside such a file is deliberately not followed, so a worktree
/// is its own repository and not the checkout it was made from. That is what the canvas wants:
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
