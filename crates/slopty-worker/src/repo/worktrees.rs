//! A finished task's worktree freed (`docs/decisions/projects.md`, "A finished task frees its
//! worktree").
//!
//! An agent that writes works in a git worktree of its own under its clone's
//! `.claude/worktrees/`, which outlives its task: a long project fills the disk with them. Once
//! the task settles, the server asks for it to go ([`remove`]). Nothing that is not saved
//! elsewhere is lost: a worktree with anything not committed, or with a terminal still working
//! in it, is kept and said, and its branch goes only when every commit on it landed.

use std::path::{Path, PathBuf};

use super::bundle::{self, branch_ref};
use super::{common_dir, git_dir};

/// Where Claude Code makes the worktrees of a clone, under its root.
const AGENT_WORKTREES: [&str; 2] = [".claude", "worktrees"];

/// Why a worktree was kept.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Failed {
    /// The path is no worktree an agent made under a clone.
    NotOne(String),
    /// A terminal is working in it.
    Busy(String),
    /// Something in it is not committed: what git lists, a few lines of it.
    Uncommitted(String),
    /// Anything else, in words.
    Other(String),
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOne(why) | Self::Busy(why) | Self::Uncommitted(why) | Self::Other(why) => {
                f.write_str(why)
            }
        }
    }
}

impl From<bundle::Failed> for Failed {
    fn from(failed: bundle::Failed) -> Self {
        Self::Other(failed.to_string())
    }
}

/// A worktree removed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Removed {
    /// The branch it had checked out, if one.
    pub branch: Option<String>,
    /// Whether that branch went too.
    pub branch_removed: bool,
}

/// How many of git's lines about what is not committed are kept to say so.
const SAID_LINES: usize = 5;

/// Remove the worktree at `worktree` from its clone.
///
/// It must be one an agent made under its clone's `.claude/worktrees/`, with no terminal
/// whose directory is in it (`cwds`, every live terminal's) and nothing uncommitted in it:
/// `git worktree remove` without `--force`, so git refuses what this missed. The branch it had
/// checked out goes too when every commit on it is in one of `landed` (commits or branches of
/// the clone; those that name nothing here are passed over) by patch, as `git cherry` reads
/// it, so work rebased onto the target counts as landed.
///
/// # Errors
/// [`Failed::NotOne`] for a path that is no such worktree, [`Failed::Busy`] while a terminal
/// works in it, [`Failed::Uncommitted`] for anything not committed, and [`Failed::Other`] for
/// a git that failed.
pub async fn remove(
    git: &Path,
    worktree: &Path,
    landed: &[String],
    cwds: &[PathBuf],
) -> Result<Removed, Failed> {
    let (tree, clone, branch) = {
        let worktree = worktree.to_path_buf();
        tokio::task::spawn_blocking(move || agent_worktree(&worktree))
            .await
            .map_err(|e| Failed::Other(e.to_string()))??
    };
    if let Some(cwd) = cwds.iter().find(|cwd| inside(cwd, &tree)) {
        return Err(Failed::Busy(format!(
            "a terminal works in {} ({})",
            tree.display(),
            cwd.display()
        )));
    }
    let status = bundle::run(git, &tree, &["status", "--porcelain"]).await?;
    if !status.trim().is_empty() {
        let lines: Vec<&str> = status.lines().take(SAID_LINES).collect();
        return Err(Failed::Uncommitted(format!(
            "{} has changes not committed: {}",
            tree.display(),
            lines.join("; ")
        )));
    }
    let tree_text = tree.to_string_lossy();
    bundle::run(git, &clone, &["worktree", "remove", "--end-of-options", &tree_text]).await?;
    let branch_removed = match &branch {
        Some(branch) if landed_in(git, &clone, branch, landed).await => {
            let args = ["branch", "-D", "--end-of-options", branch.as_str()];
            match bundle::run(git, &clone, &args).await {
                Ok(_) => true,
                Err(e) => {
                    tracing::info!(%branch, error = %e, "a landed branch not removed");
                    false
                }
            }
        }
        _ => false,
    };
    Ok(Removed { branch, branch_removed })
}

/// `worktree`, resolved, if it is a linked worktree under its clone's `.claude/worktrees/`:
/// itself, the clone's root, and the branch it has checked out.
fn agent_worktree(worktree: &Path) -> Result<(PathBuf, PathBuf, Option<String>), Failed> {
    let not_one = |why: &str| Failed::NotOne(format!("{} {why}", worktree.display()));
    let tree =
        std::fs::canonicalize(worktree).map_err(|e| not_one(&format!("is not there: {e}")))?;
    let linked = std::fs::symlink_metadata(tree.join(".git")).is_ok_and(|m| m.is_file());
    let own = git_dir(&tree).filter(|_| linked).ok_or_else(|| not_one("is no linked worktree"))?;
    let common = common_dir(&tree).ok_or_else(|| not_one("names no repository"))?;
    let common =
        std::fs::canonicalize(common).map_err(|e| not_one(&format!("names no repository: {e}")))?;
    let clone = common
        .file_name()
        .is_some_and(|name| name == ".git")
        .then(|| common.parent().map(Path::to_path_buf))
        .flatten()
        .ok_or_else(|| not_one("is not of a clone with a checkout"))?;
    let place = AGENT_WORKTREES.iter().fold(clone.clone(), |dir, part| dir.join(part));
    if tree.parent() != Some(place.as_path()) {
        return Err(not_one("is not an agent's worktree under .claude/worktrees"));
    }
    let head = std::fs::read_to_string(own.join("HEAD")).unwrap_or_default();
    let branch = head
        .trim()
        .strip_prefix("ref: refs/heads/")
        .filter(|b| branch_ref(b).is_ok())
        .map(str::to_owned);
    Ok((tree, clone, branch))
}

/// Whether `cwd` is `tree` or under it, by the path resolved where it still resolves.
fn inside(cwd: &Path, tree: &Path) -> bool {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    cwd.starts_with(tree)
}

/// Whether every commit on `branch` is in one of `landed` by patch.
async fn landed_in(git: &Path, clone: &Path, branch: &str, landed: &[String]) -> bool {
    for upstream in landed {
        let spec = format!("{upstream}^{{commit}}");
        let resolve = ["rev-parse", "--verify", "--quiet", "--end-of-options", &spec];
        let Ok(commit) = bundle::run(git, clone, &resolve).await else { continue };
        let Ok(cherry) = bundle::run(git, clone, &["cherry", commit.trim(), branch]).await else {
            continue;
        };
        if !cherry.lines().any(|line| line.starts_with('+')) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let git = crate::changes::git().expect("git");
        let out = std::process::Command::new(git)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// An agent's worktree `name` of `clone` on its own branch from `main`, with one commit.
    fn agent_tree(clone: &Path, name: &str) -> (PathBuf, String) {
        let tree = clone.join(".claude/worktrees").join(name);
        let branch = format!("worktree-{name}");
        let tree_text = tree.to_string_lossy().into_owned();
        git_in(clone, &["worktree", "add", "-q", "-b", &branch, &tree_text, "main"]);
        std::fs::write(tree.join(format!("{name}.txt")), name).expect("write");
        git_in(&tree, &["add", "."]);
        git_in(&tree, &["commit", "-q", "-m", name]);
        (tree, branch)
    }

    fn has_branch(clone: &Path, branch: &str) -> bool {
        !git_in(clone, &["branch", "--list", branch]).is_empty()
    }

    /// A clean worktree goes; its branch goes with it once its work landed on `main` rebased,
    /// and stays while it has a commit `main` lacks.
    #[tokio::test]
    async fn a_clean_worktree_goes_and_its_branch_only_once_landed() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&clone).expect("mkdir");
        git_in(&clone, &["init", "-q", "-b", "main"]);
        git_in(&clone, &["commit", "-q", "--allow-empty", "-m", "c0"]);
        let (merged, merged_branch) = agent_tree(&clone, "slopty-p-1");
        let (open, open_branch) = agent_tree(&clone, "slopty-p-2");
        // `main` moves on, then takes task 1's work rebased: new commits, the same patch.
        git_in(&clone, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        git_in(&clone, &["cherry-pick", &merged_branch]);
        let head = git_in(&clone, &["rev-parse", "main"]);
        let landed = vec![head, "main".to_owned(), "origin/main".to_owned()];

        let removed = remove(git, &merged, &landed, &[]).await.expect("removed");
        assert_eq!(removed, Removed { branch: Some(merged_branch.clone()), branch_removed: true });
        assert!(!merged.exists());
        assert!(!has_branch(&clone, &merged_branch));

        let kept = remove(git, &open, &landed, &[]).await.expect("removed");
        assert_eq!(kept, Removed { branch: Some(open_branch.clone()), branch_removed: false });
        assert!(!open.exists());
        assert!(has_branch(&clone, &open_branch), "its work is on its branch alone");
    }

    /// A worktree with a terminal in it, or anything not committed, is kept; the checkout
    /// itself, a path elsewhere and one that is not there are no agent's worktree.
    #[tokio::test]
    async fn a_worktree_in_use_or_not_committed_is_kept() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&clone).expect("mkdir");
        git_in(&clone, &["init", "-q", "-b", "main"]);
        git_in(&clone, &["commit", "-q", "--allow-empty", "-m", "c0"]);
        let (tree, _) = agent_tree(&clone, "slopty-p-3");
        let landed = ["main".to_owned()];

        let deep = tree.join("src");
        std::fs::create_dir_all(&deep).expect("mkdir");
        let busy = remove(git, &tree, &landed, &[deep]).await;
        assert!(matches!(busy, Err(Failed::Busy(_))), "{busy:?}");

        std::fs::write(tree.join("draft.txt"), "half done").expect("write");
        let dirty = remove(git, &tree, &landed, &[]).await;
        assert!(
            matches!(&dirty, Err(Failed::Uncommitted(why)) if why.contains("draft.txt")),
            "{dirty:?}"
        );
        assert!(tree.join("draft.txt").exists(), "nothing of it is lost");

        for not_one in [clone.clone(), tmp.path().to_path_buf(), tree.join("gone")] {
            let refused = remove(git, &not_one, &landed, &[]).await;
            assert!(matches!(refused, Err(Failed::NotOne(_))), "{not_one:?}: {refused:?}");
        }
        let elsewhere = tmp.path().join("elsewhere");
        let elsewhere_text = elsewhere.to_string_lossy().into_owned();
        git_in(&clone, &["worktree", "add", "-q", "-b", "mine", &elsewhere_text, "main"]);
        let refused = remove(git, &elsewhere, &landed, &[]).await;
        assert!(matches!(refused, Err(Failed::NotOne(_))), "the person's own: {refused:?}");
        assert!(elsewhere.exists());
    }
}
