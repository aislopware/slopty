//! A task's worktree made, and freed once the task is merged (`docs/decisions/projects.md`, "A
//! merged task frees its worktree", "Any agent runs a task").
//!
//! An agent that writes works in a git worktree of its own under its clone's
//! `.claude/worktrees/`. Claude Code makes its own; for any other agent the worker makes it as
//! the task's thread starts ([`make`]), the way Claude Code would, but never moving a branch
//! that is there already. It outlives its task: a long project fills the disk with them. Once
//! the task settles, the server asks for it to go ([`remove`]). Nothing that is not saved
//! elsewhere is lost: a worktree with anything not committed, or with a terminal still working
//! in it, is kept and said, and its branch goes only when every commit on it landed.
//!
//! A new worktree starts current: from its base branch as `origin` has it, fetched for a moment
//! first, unless the clone holds commits `origin` lacks (`base_of`). The files the clone's
//! `.worktreeinclude` names that git ignores (`.env`, local certificates) are copied in, as
//! Claude Code's own worktrees do, since a fresh checkout has none of them (`carry_ignored`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use slopty_proto::agent::Worktree;
use slopty_proto::thread::wire::{NewWorktree, Start};

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

/// A worktree made, or found there already ([`make`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Made {
    /// Where it is.
    pub path: PathBuf,
    /// Its branch: `worktree-<name>`.
    pub branch: String,
    /// The clone's root it was made from.
    pub clone: PathBuf,
    /// The branch the clone had checked out, if one.
    pub clone_branch: Option<String>,
}

/// How long a new worktree waits on `origin` for its base before it starts from what the clone
/// has: a start is shown at once, and a slow or unreachable remote must not hold it.
const FETCH_WAIT: Duration = Duration::from_secs(10);

/// The file in a clone's root naming the ignored files a new worktree gets a copy of, in
/// `.gitignore`'s syntax, as Claude Code reads it.
const WORKTREE_INCLUDE: &str = ".worktreeinclude";

/// Make the worktree `name` of the clone rooted at `clone`, as Claude Code's `--worktree <name>`
/// does.
///
/// It is `.claude/worktrees/<name>` on branch `worktree-<name>`, from `base` (`base_of`),
/// with the ignored files `.worktreeinclude` names copied in (`carry_ignored`). One there
/// already is reopened as it is. Unlike Claude Code's, a branch of that name there already is
/// checked out where it is, never reset to the base, so the work of a task tried again is kept.
///
/// # Errors
/// [`Failed::NotOne`] for a `clone` that is no clone's root, a `name` that is no single plain
/// name or a `base` that is no branch here or on `origin`, [`Failed::Other`] for a git that
/// failed.
pub async fn make(
    git: &Path,
    clone: &Path,
    name: &str,
    base: Option<&str>,
) -> Result<Made, Failed> {
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && branch_ref(&format!("worktree-{name}")).is_ok();
    if !plain {
        return Err(Failed::NotOne(format!("{name:?} is no worktree name")));
    }
    let clone = std::fs::canonicalize(clone)
        .map_err(|e| Failed::NotOne(format!("{} is not there: {e}", clone.display())))?;
    if super::root_of(&clone).as_deref() != Some(clone.as_path()) {
        return Err(Failed::NotOne(format!("{} is no clone's root", clone.display())));
    }
    let branch = format!("worktree-{name}");
    let path = AGENT_WORKTREES.iter().fold(clone.clone(), |dir, part| dir.join(part)).join(name);
    let clone_branch = super::branch_of(&clone);
    if path.exists() {
        let (tree, of, _) = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || agent_worktree(&path))
                .await
                .map_err(|e| Failed::Other(e.to_string()))??
        };
        if of != clone {
            return Err(Failed::NotOne(format!("{} is another clone's", tree.display())));
        }
        return Ok(Made { path: tree, branch, clone, clone_branch });
    }
    let path_text = path.to_string_lossy();
    let exists = ["rev-parse", "--verify", "--quiet", "--end-of-options", &branch_ref(&branch)?];
    let fresh = bundle::run(git, &clone, &exists).await.is_err();
    if fresh {
        let from = base_of(git, &clone, base.or(clone_branch.as_deref())).await?;
        let args =
            ["worktree", "add", "--no-track", "-b", &branch, "--end-of-options", &path_text, &from];
        bundle::run(git, &clone, &args).await?;
    } else {
        bundle::run(git, &clone, &["worktree", "add", "--end-of-options", &path_text, &branch])
            .await?;
    }
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    if fresh {
        carry_ignored(git, &clone, &path).await;
    }
    Ok(Made { path, branch, clone, clone_branch })
}

/// What a new worktree starts from: the branch `wanted`, as current as can be had at once.
///
/// `origin`'s copy is fetched for at most [`FETCH_WAIT`]. When it holds every commit of the
/// clone's own branch, the worktree starts from it (current, nothing lost); when the clone's
/// branch has commits `origin` lacks, or `origin` has no such branch or could not be reached,
/// from the clone's branch. With no branch wanted (a detached `HEAD`), from `HEAD`.
///
/// # Errors
/// [`Failed::NotOne`] for a branch neither the clone nor `origin` has.
async fn base_of(git: &Path, clone: &Path, wanted: Option<&str>) -> Result<String, Failed> {
    let Some(branch) = wanted else { return Ok("HEAD".to_owned()) };
    let local = branch_ref(branch)?;
    let has = async |name: String| {
        let commit = format!("{name}^{{commit}}");
        bundle::run(git, clone, &["rev-parse", "--verify", "--quiet", "--end-of-options", &commit])
            .await
            .is_ok()
    };
    let is_branch = has(local.clone()).await;
    // A detached clone is named by its commit, which is no branch: it starts from `HEAD`.
    if !is_branch && branch.len() >= 7 && branch.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok("HEAD".to_owned());
    }
    let remote = format!("refs/remotes/origin/{branch}");
    if bundle::run(git, clone, &["remote", "get-url", "origin"]).await.is_ok() {
        let spec = format!("+{local}:{remote}");
        let fetch = ["fetch", "--quiet", "--no-tags", "--no-recurse-submodules", "origin", &spec];
        if let Err(failed) = bundle::run_within(git, clone, &fetch, FETCH_WAIT).await {
            tracing::info!(%failed, branch, "a worktree starts from the clone's copy");
        }
    }
    let on_origin = has(remote.clone()).await;
    match (is_branch, on_origin) {
        (true, true) => {
            let behind = ["merge-base", "--is-ancestor", "--end-of-options", &local, &remote];
            let current = bundle::run(git, clone, &behind).await.is_ok();
            Ok(if current { remote } else { local })
        }
        (true, false) => Ok(local),
        (false, true) => Ok(remote),
        (false, false) => Err(Failed::NotOne(format!("{branch} is no branch here or on origin"))),
    }
}

/// Copy into the new worktree at `tree` the files of `clone` its `.worktreeinclude` names
/// (`.gitignore` syntax) that git ignores there: a fresh checkout has the tracked files, and an
/// untracked file git does not ignore would show as a change. Nothing is overwritten, and a
/// file that cannot be copied is passed over: the worktree is made either way.
async fn carry_ignored(git: &Path, clone: &Path, tree: &Path) {
    let include = clone.join(WORKTREE_INCLUDE);
    if !include.is_file() {
        return;
    }
    let from = format!("--exclude-from={}", include.display());
    let listed = ["ls-files", "-z", "--others", "--ignored", "--exclude-standard"];
    let named = ["ls-files", "-z", "--others", "--ignored", &from];
    let (ignored, wanted) =
        tokio::join!(bundle::run(git, clone, &listed), bundle::run(git, clone, &named));
    let (Ok(ignored), Ok(wanted)) = (ignored, wanted) else { return };
    let ignored: std::collections::HashSet<&str> =
        ignored.split('\0').filter(|p| !p.is_empty()).collect();
    let paths: Vec<PathBuf> = wanted
        .split('\0')
        .filter(|p| !p.is_empty() && ignored.contains(p))
        .map(PathBuf::from)
        // `.claude/worktrees` holds the other worktrees: never copied into this one.
        .filter(|p| !p.starts_with(AGENT_WORKTREES.iter().collect::<PathBuf>()))
        .collect();
    let (clone, tree) = (clone.to_path_buf(), tree.to_path_buf());
    let copied = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .filter(|path| {
                let to = tree.join(path);
                !to.exists()
                    && to.parent().is_none_or(|dir| std::fs::create_dir_all(dir).is_ok())
                    && std::fs::copy(clone.join(path), &to).is_ok()
            })
            .count()
    })
    .await;
    if let Ok(copied) = copied {
        tracing::info!(copied, "ignored files carried into a new worktree");
    }
}

/// Move `start` into the worktree it names ([`Start::worktree`]), made or reopened by [`make`]
/// from the clone its `cwd` is in, and say where it is. A start that names none is left as it
/// is.
///
/// The clone is the main checkout of the repository `cwd` is in, so a start from inside another
/// worktree makes one beside it, never one nested in it. The agent starts where `cwd` stood in
/// the clone when that folder is in the worktree too, else at its root. The worktree is trusted
/// for the agent as the clones the server makes are ([`super::cloning::trust`]), which passes
/// over one outside them.
///
/// # Errors
/// [`Failed::NotOne`] for a `cwd` in no repository or a name that is no plain name, and
/// [`Failed::Other`] for a machine with no git or a git that failed.
pub async fn enter(start: &mut Start) -> Result<Option<Worktree>, Failed> {
    let Some(asked) = start.worktree.take() else { return Ok(None) };
    let (at, made) = open(&start.cwd, asked).await?;
    start.cwd = at;
    Ok(Some(made))
}

/// Make or reopen the worktree `asked` names ([`make`]) from the clone `cwd` is in.
///
/// It is trusted for the agent, and the answer says where `cwd` stands in it (its root when
/// that folder is not in it) and what it is. [`enter`] starts a thread there; a spawned
/// agent's own `--worktree <name>` opens it.
///
/// # Errors
/// As [`enter`].
pub async fn open(cwd: &str, asked: NewWorktree) -> Result<(String, Worktree), Failed> {
    let NewWorktree { name, base } = asked;
    let git =
        crate::changes::git().ok_or_else(|| Failed::Other("this machine has no git".to_owned()))?;
    let cwd = crate::file::expand_home(Path::new(cwd));
    let (clone, within) = {
        let cwd = cwd.clone();
        tokio::task::spawn_blocking(move || clone_of(&cwd))
            .await
            .map_err(|e| Failed::Other(e.to_string()))??
    };
    let made = make(git, &clone, &name, base.as_deref()).await?;
    let home = slopty_platform::dirs::home();
    let (at, path) = (made.path.join(&within), made.path.clone());
    let at = tokio::task::spawn_blocking(move || {
        super::cloning::trust(&home, &path);
        if at.is_dir() { at } else { path }
    })
    .await
    .map_err(|e| Failed::Other(e.to_string()))?;
    let text = |p: &Path| p.to_string_lossy().into_owned();
    let worktree = Worktree {
        name,
        path: text(&made.path),
        branch: Some(made.branch),
        original_cwd: text(&made.clone),
        original_branch: made.clone_branch,
    };
    Ok((text(&at), worktree))
}

/// The main checkout of the repository `cwd` is in, and where `cwd` stands in its own
/// checkout. A worktree's main checkout is the one its git directory is shared from; a
/// submodule's, whose shared directory is no checkout's `.git`, is its own.
fn clone_of(cwd: &Path) -> Result<(PathBuf, PathBuf), Failed> {
    let root = super::root_of(cwd)
        .ok_or_else(|| Failed::NotOne(format!("{} is in no git repository", cwd.display())))?;
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let within = resolved.strip_prefix(&root).map(Path::to_path_buf).unwrap_or_default();
    let linked = std::fs::symlink_metadata(root.join(".git")).is_ok_and(|m| m.is_file());
    let main = linked
        .then(|| common_dir(&root).and_then(|c| std::fs::canonicalize(c).ok()))
        .flatten()
        .filter(|common| common.file_name().is_some_and(|name| name == ".git"))
        .and_then(|common| common.parent().map(Path::to_path_buf));
    Ok((main.unwrap_or(root), within))
}

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

/// Free the worktree at `worktree` for the person, as [`remove`] does for a task.
///
/// What counts as landed is read from its clone: `origin`'s default branch and the branch the
/// clone has checked out. A squash or a rebase merge leaves no commit `git cherry` matches, so a
/// branch whose pull request merged at the commit it ends at, as the person's own `gh` says, counts
/// as landed too. gh is asked only when the commits alone do not say so; without it the branch
/// stays.
///
/// # Errors
/// As [`remove`].
pub async fn free(
    git: &Path,
    gh: Option<&Path>,
    worktree: &Path,
    terminals: &[PathBuf],
) -> Result<Removed, Failed> {
    let (tree, clone, branch) = {
        let worktree = worktree.to_path_buf();
        tokio::task::spawn_blocking(move || agent_worktree(&worktree))
            .await
            .map_err(|e| Failed::Other(e.to_string()))??
    };
    let mut landed = vec![DEFAULT_BRANCH.to_owned(), "HEAD".to_owned()];
    if let Some(branch) = branch.filter(|_| gh.is_some())
        && !landed_in(git, &clone, &branch, &landed).await
        && merged_at_tip(git, gh, &tree).await
    {
        landed.push(branch);
    }
    remove(git, &tree, &landed, terminals).await
}

/// How `origin`'s default branch is named in a clone.
const DEFAULT_BRANCH: &str = "origin/HEAD";

/// Whether the pull request of the branch checked out at `tree` merged at the commit the
/// branch ends at, as `gh` reads it. Anything gh cannot say is a no.
async fn merged_at_tip(git: &Path, gh: Option<&Path>, tree: &Path) -> bool {
    let Ok(tip) = bundle::run(git, tree, &["rev-parse", "HEAD"]).await else { return false };
    let Ok(Some(pull)) = super::pull::status(gh, tree).await else { return false };
    pull.state == "MERGED" && pull.head_commit == tip.trim()
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

    /// A worktree is made where Claude Code's `--worktree` makes one, on its branch from
    /// `HEAD` with no `origin`; made again it is the same one, as it is; a branch of its name
    /// left from before is checked out where it stands, never reset; and a name that is no
    /// plain name, or a folder that is no clone's root, makes nothing.
    #[tokio::test]
    async fn a_worktree_is_made_as_claude_code_would_and_a_branch_there_is_kept() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&clone).expect("mkdir");
        git_in(&clone, &["init", "-q", "-b", "main"]);
        git_in(&clone, &["commit", "-q", "--allow-empty", "-m", "c0"]);
        let head = git_in(&clone, &["rev-parse", "HEAD"]);

        let made = make(git, &clone, "slopty-p-1", None).await.expect("made");
        let root = std::fs::canonicalize(&clone).expect("canonical");
        assert_eq!(made.path, root.join(".claude/worktrees/slopty-p-1"));
        assert_eq!(
            (made.branch.as_str(), made.clone_branch.as_deref()),
            ("worktree-slopty-p-1", Some("main"))
        );
        assert_eq!(git_in(&made.path, &["rev-parse", "HEAD"]), head);
        std::fs::write(made.path.join("half.txt"), "kept").expect("write");
        let again = make(git, &clone, "slopty-p-1", None).await.expect("found");
        assert_eq!(again.path, made.path);
        assert!(again.path.join("half.txt").exists(), "reopened as it is");

        let (tree, branch) = agent_tree(&clone, "slopty-p-2");
        let work = git_in(&tree, &["rev-parse", "HEAD"]);
        git_in(&clone, &["worktree", "remove", "--force", &tree.to_string_lossy()]);
        let tried_again = make(git, &clone, "slopty-p-2", None).await.expect("made");
        assert_eq!(tried_again.branch, branch);
        assert_eq!(git_in(&tried_again.path, &["rev-parse", "HEAD"]), work, "its work kept");

        for name in ["", "../out", "a/b", ".hidden", "with space"] {
            let refused = make(git, &clone, name, None).await;
            assert!(matches!(refused, Err(Failed::NotOne(_))), "{name:?}: {refused:?}");
        }
        let inside = make(git, &clone.join(".claude"), "slopty-p-3", None).await;
        assert!(matches!(inside, Err(Failed::NotOne(_))), "{inside:?}");
    }

    /// A new worktree starts from its branch as `origin` has it, fetched first, unless the
    /// clone holds commits `origin` lacks; from a branch only `origin` has; from `HEAD` when
    /// the clone is detached; and not at all from a branch neither has. It gets a copy of the
    /// ignored files `.worktreeinclude` names, and nothing else untracked.
    #[tokio::test]
    async fn a_new_worktree_starts_current_and_carries_the_ignored_files_it_names() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let (origin, clone, other) =
            (tmp.path().join("origin.git"), tmp.path().join("clone"), tmp.path().join("other"));
        git_in(tmp.path(), &["init", "-q", "--bare", "-b", "main", &origin.to_string_lossy()]);
        git_in(tmp.path(), &["clone", "-q", &origin.to_string_lossy(), &clone.to_string_lossy()]);
        git_in(&clone, &["commit", "-q", "--allow-empty", "-m", "c0"]);
        git_in(&clone, &["push", "-q", "origin", "main"]);
        git_in(tmp.path(), &["clone", "-q", &origin.to_string_lossy(), &other.to_string_lossy()]);
        git_in(&other, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        git_in(&other, &["push", "-q", "origin", "main"]);
        let c1 = git_in(&other, &["rev-parse", "HEAD"]);
        git_in(&other, &["push", "-q", "origin", "main:feature"]);

        std::fs::write(clone.join(".gitignore"), ".env\ncerts/\nsecret.txt\n").expect("write");
        std::fs::write(clone.join(".worktreeinclude"), ".env\ncerts/\nnotes.txt\n").expect("write");
        std::fs::create_dir_all(clone.join("certs")).expect("mkdir");
        for (file, text) in
            [(".env", "KEY=1"), ("certs/dev.pem", "pem"), ("secret.txt", "no"), ("notes.txt", "no")]
        {
            std::fs::write(clone.join(file), text).expect("write");
        }
        let behind = make(git, &clone, "behind", None).await.expect("made");
        assert_eq!(git_in(&behind.path, &["rev-parse", "HEAD"]), c1, "origin's, fetched");
        let read = |file: &str| std::fs::read_to_string(behind.path.join(file)).ok();
        assert_eq!(read(".env").as_deref(), Some("KEY=1"));
        assert_eq!(read("certs/dev.pem").as_deref(), Some("pem"));
        assert_eq!(read("secret.txt"), None, "ignored but not named");
        assert_eq!(read("notes.txt"), None, "named but not ignored: it would show as a change");

        git_in(&clone, &["pull", "-q", "--ff-only", "origin", "main"]);
        git_in(&clone, &["commit", "-q", "--allow-empty", "-m", "c2 not pushed"]);
        let c2 = git_in(&clone, &["rev-parse", "HEAD"]);
        let ahead = make(git, &clone, "ahead", None).await.expect("made");
        assert_eq!(git_in(&ahead.path, &["rev-parse", "HEAD"]), c2, "the work not pushed kept");

        let feature = make(git, &clone, "feature", Some("feature")).await.expect("made");
        assert_eq!(git_in(&feature.path, &["rev-parse", "HEAD"]), c1, "a branch origin alone has");
        let none = make(git, &clone, "none", Some("nowhere")).await;
        assert!(matches!(none, Err(Failed::NotOne(_))), "{none:?}");

        git_in(&clone, &["checkout", "-q", "--detach", "HEAD~1"]);
        let detached = make(git, &clone, "detached", None).await.expect("made");
        assert_eq!(git_in(&detached.path, &["rev-parse", "HEAD"]), c1, "from HEAD");
    }

    /// A start naming a worktree moves into it, made from the main checkout of the repository
    /// its folder is in, and stands where the folder stood; one started from inside another
    /// worktree gets one beside it. A start naming none is left as it is, and one whose folder
    /// is in no repository makes nothing.
    #[tokio::test]
    async fn a_start_enters_its_worktree_where_its_folder_stood() {
        use slopty_proto::thread::AgentId;
        if crate::changes::git().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().expect("temp");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(clone.join("web/src")).expect("mkdir");
        git_in(&clone, &["init", "-q", "-b", "main"]);
        std::fs::write(clone.join("web/src/app.ts"), "app").expect("write");
        git_in(&clone, &["add", "."]);
        git_in(&clone, &["commit", "-q", "-m", "c0"]);
        let root = std::fs::canonicalize(&clone).expect("canonical");
        let start = |cwd: &Path, worktree: Option<&str>| Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: cwd.to_string_lossy().into_owned(),
            drive: None,
            prompt: None,
            model: None,
            mode: None,
            effort: None,
            attachments: Vec::new(),
            args: Vec::new(),
            worktree: worktree.map(NewWorktree::named),
        };

        let mut plain = start(&clone, None);
        assert_eq!(enter(&mut plain).await.expect("nothing to make"), None);
        assert_eq!(plain, start(&clone, None), "left as it is");

        let mut deep = start(&clone.join("web"), Some("claude-1"));
        let made = enter(&mut deep).await.expect("made").expect("a worktree");
        let tree = root.join(".claude/worktrees/claude-1");
        assert_eq!(made.path, tree.to_string_lossy());
        assert_eq!(made.original_cwd, root.to_string_lossy());
        assert_eq!(
            (made.branch.as_deref(), made.original_branch.as_deref()),
            (Some("worktree-claude-1"), Some("main"))
        );
        assert_eq!(deep.cwd, tree.join("web").to_string_lossy(), "where the folder stood");
        assert_eq!(deep.worktree, None, "taken by the worker");

        let mut beside = start(&tree.join("web/src"), Some("claude-2"));
        let made = enter(&mut beside).await.expect("made").expect("a worktree");
        let sibling = root.join(".claude/worktrees/claude-2");
        assert_eq!(made.path, sibling.to_string_lossy(), "beside it, not inside it");
        assert_eq!(beside.cwd, sibling.join("web/src").to_string_lossy());

        let elsewhere = tmp.path().join("notes");
        std::fs::create_dir_all(&elsewhere).expect("mkdir");
        let refused = enter(&mut start(&elsewhere, Some("claude-3"))).await;
        assert!(
            matches!(&refused, Err(Failed::NotOne(why)) if why.contains("no git repository")),
            "{refused:?}"
        );
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

    /// A stand-in for gh in `dir`, as gh 2.102 answers from inside a linked worktree: `pr merge`
    /// merges, deletes the remote branch, and skips the local one checked out there with a
    /// warning; `pr view` says the pull request ends at the commit in `dir/tip` and stands as
    /// `dir/state` says, `MERGED` once merged. It writes where it ran and what it was asked to
    /// `dir/asked`. No real gh runs, so no one's GitHub sign-in is reached.
    fn stand_in_gh(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let gh = dir.join("gh");
        std::fs::write(dir.join("state"), "OPEN").expect("written");
        let script = format!(
            "#!/bin/sh\nd=\"{dir}\"\nprintf '%s: %s\\n' \"$(pwd -P)\" \"$*\" >> \"$d/asked\"\n\
             case \"$1 $2\" in\n\
             'pr merge') echo MERGED > \"$d/state\"\n\
               echo \"! Branch is checked out in the current worktree ($(pwd -P)); skipping local delete\" >&2\n\
               echo '✓ Squashed and merged pull request #7' ;;\n\
             'pr view') printf '{{\"number\":7,\"state\":\"%s\",\"headRefOid\":\"%s\"}}\\n' \
               \"$(cat \"$d/state\")\" \"$(cat \"$d/tip\")\" ;;\n\
             *) echo \"unexpected: $*\" >&2; exit 2 ;;\n\
             esac\n",
            dir = dir.display()
        );
        std::fs::write(&gh, script).expect("written");
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("executable");
        gh
    }

    /// The person's own worktree, made by "New worktree of", goes the way it would by hand:
    /// refused in words while a terminal works in it or anything in it is not committed; its
    /// pull request merged with `--delete-branch` from inside it, where gh leaves the local
    /// branch checked out there; then freed, and its branch with it because gh says its pull
    /// request merged at its tip, though the squash left no commit `git cherry` would match. A
    /// worktree whose work has not landed goes and leaves its branch, so no commit is lost.
    #[tokio::test]
    async fn a_persons_worktree_is_freed_after_its_pull_request_merges_from_inside_it() {
        use slopty_proto::git::{GitDone, GitOp, GitOutcome};

        use crate::repo::commit::{Programs, apply};

        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&clone).expect("mkdir");
        git_in(&clone, &["init", "-q", "-b", "main"]);
        git_in(&clone, &["commit", "-q", "--allow-empty", "-m", "c0"]);
        let made = make(git, &clone, "fix-login", None).await.expect("made");
        let tree = made.path.clone();
        for step in ["one", "two"] {
            std::fs::write(tree.join(format!("{step}.txt")), step).expect("write");
            git_in(&tree, &["add", "."]);
            git_in(&tree, &["commit", "-q", "-m", step]);
        }
        std::fs::write(tmp.path().join("tip"), git_in(&tree, &["rev-parse", "HEAD"]))
            .expect("write");
        let programs = Programs { git: Some(git.to_path_buf()), gh: Some(stand_in_gh(tmp.path())) };
        let at = tree.to_string_lossy().into_owned();
        let free = |terminals: Vec<PathBuf>| {
            let (programs, at) = (programs.clone(), at.clone());
            async move { apply(&programs, &at, GitOp::RemoveWorktree, &terminals).await }
        };

        let busy = free(vec![tree.join("src")]).await;
        assert!(
            matches!(&busy, GitOutcome::Refused { why } if why.contains("a terminal works in")),
            "{busy:?}"
        );
        std::fs::write(tree.join("draft.txt"), "half done").expect("write");
        let dirty = free(Vec::new()).await;
        assert!(
            matches!(&dirty, GitOutcome::Refused { why } if why.contains("draft.txt")),
            "{dirty:?}"
        );
        std::fs::remove_file(tree.join("draft.txt")).expect("removed");

        let merge = GitOp::Merge { method: "squash".to_owned(), head: None, delete_branch: true };
        let merged = apply(&programs, &at, merge, &[]).await;
        assert!(matches!(&merged, GitOutcome::Done(GitDone::Merged { .. })), "{merged:?}");
        let asked = std::fs::read_to_string(tmp.path().join("asked")).expect("asked");
        assert!(
            asked.lines().any(|l| l == format!("{at}: pr merge --squash --delete-branch")),
            "gh merged from inside the worktree: {asked}"
        );
        // The forge squashes the branch into `main`: one new commit, neither of the branch's.
        git_in(&clone, &["merge", "-q", "--squash", &made.branch]);
        git_in(&clone, &["commit", "-q", "-m", "Fix the login (#7)"]);

        let freed = free(Vec::new()).await;
        let GitOutcome::Done(GitDone::WorktreeRemoved { branch, branch_removed }) = freed else {
            panic!("not freed: {freed:?}")
        };
        assert_eq!((branch.as_deref(), branch_removed), (Some(made.branch.as_str()), true));
        assert!(!tree.exists());
        assert!(!has_branch(&clone, &made.branch));

        let (open, open_branch) = agent_tree(&clone, "half-done");
        let alone = Programs { git: Some(git.to_path_buf()), gh: None };
        let kept = apply(&alone, &open.to_string_lossy(), GitOp::RemoveWorktree, &[]).await;
        let GitOutcome::Done(GitDone::WorktreeRemoved { branch_removed, .. }) = kept else {
            panic!("not freed: {kept:?}")
        };
        assert!(!branch_removed);
        assert!(!open.exists());
        assert!(has_branch(&clone, &open_branch), "its commit is on its branch alone");
    }
}
