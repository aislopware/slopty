//! A project's work verified and merged in the orchestrator's clone
//! (`docs/decisions/projects.md`, "A task is done when its verifier passes").
//!
//! Each project has one checkout of the clone for this: a `git worktree` under
//! [`slopty_proto::project::VERIFY_PLACES`], detached, made on first use and reused, so what a
//! verifier builds there (a `target/` directory) stays warm between runs ([`checkout`]). Neither
//! the person's checkout nor an agent's is ever touched to verify.
//!
//! The merge queue rebases a task's work onto the target branch in that same checkout
//! ([`rebase`]), each commit carrying where it came from as trailers, so the verifier then runs
//! on exactly the commit the target will point at, and moves the target to it only as a
//! fast-forward from the commit it was rebased onto ([`fast_forward`]). What a task's work did
//! to the tests is read from the same clone ([`test_diff`]). Where the target is checked out, that
//! checkout moves through `git merge --ff-only`, which refuses to overwrite anything of the
//! person's there; elsewhere the ref moves by compare-and-swap. Nothing is ever force-moved or
//! force-pushed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use slopty_proto::project::{TESTS_NAMED, TestDiff, is_test_path};

use super::bundle::{self, branch_ref};

/// Why a checkout, a rebase or a fast-forward did not happen.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Failed {
    /// The rebase stopped on conflicts in these paths, and was undone.
    Conflict(Vec<String>),
    /// The target branch is no longer where it was asked to move from: it is at this commit.
    Moved(String),
    /// Anything else, in words.
    Other(String),
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(paths) => write!(f, "conflicts in {}", paths.join(", ")),
            Self::Moved(at) => write!(f, "the branch moved to {at}"),
            Self::Other(why) => f.write_str(why),
        }
    }
}

impl From<bundle::Failed> for Failed {
    fn from(failed: bundle::Failed) -> Self {
        Self::Other(failed.to_string())
    }
}

/// A checkout ready to verify in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Checkout {
    /// Where it is.
    pub path: PathBuf,
    /// The commit checked out, in hex.
    pub head: String,
    /// Where it left the target branch, in hex.
    pub base: String,
}

/// A rebase's result.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rebased {
    /// What it made, in hex; the head itself when that already held the target and nothing
    /// was to be added to its commits.
    pub head: String,
    /// The target's commit it is on top of, in hex.
    pub onto: String,
    /// What it made has the very tree of the commit last verified: only the messages changed.
    pub verified: bool,
}

/// A fast-forward's result.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Moved {
    /// The branch's commit now, in hex.
    pub head: String,
    /// It was pushed to `origin`.
    pub pushed: bool,
    /// Why a push asked for did not happen.
    pub push_failed: Option<String>,
}

/// The project's checkout in `places`, by its name: one path component and no more.
///
/// # Errors
/// A name that is empty, hidden, or not a plain file name.
pub fn place(places: &Path, name: &str) -> Result<PathBuf, Failed> {
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if plain {
        Ok(places.join(name))
    } else {
        Err(Failed::Other(format!("{name:?} is not a checkout's name")))
    }
}

/// `head` checked out in the project's checkout at `place` of the clone at `repo`, with its
/// fork point from the branch `target`.
///
/// # Errors
/// A head or target that is not there, a head that shares no history with the target, or a
/// git that failed.
pub async fn checkout(
    git: &Path,
    repo: &Path,
    place: &Path,
    head: &str,
    target: &str,
) -> Result<Checkout, Failed> {
    let head = commit_of(git, repo, head).await?;
    let onto = commit_of(git, repo, &branch_ref(target)?).await?;
    let base = bundle::run(git, repo, &["merge-base", "--end-of-options", &head, &onto])
        .await
        .map_err(|why| {
            Failed::Other(format!("{} shares no history with {target}: {why}", short(&head)))
        })?
        .trim()
        .to_owned();
    prepare(git, repo, place, &head).await?;
    Ok(Checkout { path: place.to_path_buf(), head, base })
}

/// `head` rebased onto the branch `onto` in the project's checkout at `place`, as the merge
/// queue takes it.
///
/// Every commit it adds to `onto` carries each of `trailers` (a token and its value) once, and
/// the answer says whether what it made has the tree of the commit `verified`.
///
/// A head that already holds `onto` with no trailers to add is answered as it is; any other is
/// left checked out there, rebased, for the verifier to run on.
///
/// # Errors
/// [`Failed::Conflict`] with the paths when it does not apply cleanly (the rebase is undone); a
/// trailer that is not a plain token and a one-line value; otherwise a git that failed, such as
/// a commit it could not sign.
pub async fn rebase(
    git: &Path,
    repo: &Path,
    place: &Path,
    (head, onto): (&str, &str),
    trailers: &[(String, String)],
    verified: Option<&str>,
) -> Result<Rebased, Failed> {
    let amend = amend_with(trailers)?;
    let head = commit_of(git, repo, head).await?;
    let onto = commit_of(git, repo, &branch_ref(onto)?).await?;
    let holds = is_ancestor(git, repo, &onto, &head).await;
    if holds && (amend.is_none() || head == onto) {
        let verified = same_tree(git, repo, &head, verified).await;
        return Ok(Rebased { head, onto, verified });
    }
    prepare(git, repo, place, &head).await?;
    let mut args = identity(git, place).await;
    args.extend(["rebase", "--no-autosquash", "--no-update-refs", "--quiet"].map(str::to_owned));
    if let Some(amend) = amend {
        // Every commit picked again, those already on top of `onto` too, so each takes them.
        args.extend(["--force-rebase".to_owned(), "--exec".to_owned(), amend]);
    }
    args.extend(["--end-of-options".to_owned(), onto.clone()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(stopped) = bundle::run(git, place, &args).await {
        let listed = bundle::run(git, place, &["diff", "--name-only", "--diff-filter=U"]).await;
        let conflicts: Vec<String> = listed
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        abort_rebase(git, place).await;
        return Err(if conflicts.is_empty() {
            Failed::Other(stopped.to_string())
        } else {
            Failed::Conflict(conflicts)
        });
    }
    let head = commit_of(git, place, "HEAD").await?;
    let verified = same_tree(git, place, &head, verified).await;
    Ok(Rebased { head, onto, verified })
}

/// The command `git rebase --exec` runs after each commit to add `trailers` to it, none when
/// there are none, each skipped where the commit carries it already. A token is letters, digits
/// and `-`; a value is one line with no quote, so the shell takes each as it is.
fn amend_with(trailers: &[(String, String)]) -> Result<Option<String>, Failed> {
    if trailers.is_empty() {
        return Ok(None);
    }
    let mut command = "git -c trailer.ifexists=addIfDifferent commit --amend --no-edit \
                       --no-verify --allow-empty --quiet"
        .to_owned();
    for (token, value) in trailers {
        let plain_token =
            !token.is_empty() && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let plain_value =
            !value.trim().is_empty() && !value.chars().any(|c| c.is_control() || c == '\'');
        if !plain_token || !plain_value {
            return Err(Failed::Other(format!("{token:?}: {value:?} is not a trailer")));
        }
        let _written = write!(command, " --trailer '{token}: {}'", value.trim());
    }
    Ok(Some(command))
}

/// Whether `commit` has the very tree of `verified`.
async fn same_tree(git: &Path, repo: &Path, commit: &str, verified: Option<&str>) -> bool {
    let Some(verified) = verified else { return false };
    let tree = async |what: &str| {
        let spec = format!("{what}^{{tree}}");
        bundle::run(git, repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec])
            .await
            .ok()
            .map(|t| t.trim().to_owned())
    };
    match (tree(commit).await, tree(verified).await) {
        (Some(ours), Some(theirs)) => ours == theirs,
        _ => false,
    }
}

/// What `head` did to the tests since it left the branch `target`, in the clone at `repo`.
///
/// That is the test files ([`is_test_path`], with the project's `test_paths`) it deleted,
/// changed or renamed, and added, from `git diff --name-status` between their fork point and
/// `head`.
///
/// # Errors
/// A head or target that is not there, one that shares no history with the other, or a git
/// that failed.
pub async fn test_diff(
    git: &Path,
    repo: &Path,
    head: &str,
    target: &str,
    test_paths: &[String],
) -> Result<TestDiff, Failed> {
    let head = commit_of(git, repo, head).await?;
    let onto = commit_of(git, repo, &branch_ref(target)?).await?;
    let base =
        bundle::run(git, repo, &["merge-base", "--end-of-options", &onto, &head]).await.map_err(
            |why| Failed::Other(format!("{} shares no history with {target}: {why}", short(&head))),
        )?;
    let listed = bundle::run(
        git,
        repo,
        &[
            "diff",
            "--name-status",
            "-z",
            "-M",
            "--no-color",
            "--end-of-options",
            base.trim(),
            &head,
        ],
    )
    .await?;
    Ok(tests_in(&listed, head, test_paths))
}

/// The test files `git diff --name-status -z` listed, at `head`.
fn tests_in(listed: &str, head: String, test_paths: &[String]) -> TestDiff {
    let mut diff = TestDiff { head, ..TestDiff::default() };
    let test = |path: &str| is_test_path(path, test_paths);
    let name = |list: &mut Vec<String>, count: &mut u16, path: &str| {
        *count = count.saturating_add(1);
        if list.len() < TESTS_NAMED {
            list.push(path.to_owned());
        }
    };
    let mut fields = listed.split('\0').filter(|f| !f.is_empty());
    while let Some(status) = fields.next() {
        let Some(path) = fields.next() else { break };
        match status.chars().next() {
            Some('D') if test(path) => name(&mut diff.deleted, &mut diff.deleted_count, path),
            Some('A') if test(path) => diff.added_count = diff.added_count.saturating_add(1),
            Some('M' | 'T') if test(path) => {
                name(&mut diff.changed, &mut diff.changed_count, path);
            }
            // A rename or a copy names the old path, then the new.
            Some(kind @ ('R' | 'C')) => {
                let Some(new) = fields.next() else { break };
                match (kind, test(path), test(new)) {
                    ('R', true, true) => name(&mut diff.changed, &mut diff.changed_count, new),
                    ('R', true, false) => name(&mut diff.deleted, &mut diff.deleted_count, path),
                    (_, _, true) => diff.added_count = diff.added_count.saturating_add(1),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    diff
}

/// Move the branch `target` of the clone at `repo` from `from` to `to`, and push it to
/// `origin` when `push` says.
///
/// # Errors
/// [`Failed::Moved`] when the branch is no longer at `from`; otherwise `to` not descending
/// from `from`, or the checkout that has the branch holding changes the move would overwrite.
pub async fn fast_forward(
    git: &Path,
    repo: &Path,
    target: &str,
    from: &str,
    to: &str,
    push: bool,
) -> Result<Moved, Failed> {
    let reference = branch_ref(target)?;
    let (from, to) = (commit_of(git, repo, from).await?, commit_of(git, repo, to).await?);
    let now = commit_of(git, repo, &reference).await?;
    if now != from {
        return Err(Failed::Moved(now));
    }
    if !is_ancestor(git, repo, &from, &to).await {
        return Err(Failed::Other(format!(
            "{} does not descend from {}",
            short(&to),
            short(&from)
        )));
    }
    // `merge --ff-only` stops, changing nothing, when the person's changes there are in its
    // way; `update-ref` would move the branch under them and leave the index stale.
    if let Some(tree) = checked_out(git, repo, &reference).await? {
        bundle::run(git, &tree, &["merge", "--ff-only", "--quiet", "--end-of-options", &to])
            .await?;
    } else {
        let why = "slopty: merge queue";
        bundle::run(git, repo, &["update-ref", "-m", why, &reference, &to, &from]).await?;
    }
    let (pushed, push_failed) = if push {
        let refspec = format!("{to}:{reference}");
        match bundle::run(git, repo, &["push", "--quiet", "--end-of-options", "origin", &refspec])
            .await
        {
            Ok(_) => (true, None),
            Err(why) => (false, Some(why.to_string())),
        }
    } else {
        (false, None)
    };
    Ok(Moved { head: to, pushed, push_failed })
}

/// The project's checkout at `place` made a worktree of the clone at `repo` if it is not one,
/// with `commit` checked out detached and nothing of an earlier run's left but what git
/// ignores.
async fn prepare(git: &Path, repo: &Path, place: &Path, commit: &str) -> Result<(), Failed> {
    let common = async |dir: PathBuf| {
        let found =
            bundle::run(git, &dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .await
                .ok()?;
        std::fs::canonicalize(found.trim()).ok()
    };
    let ours = common(repo.to_path_buf()).await;
    if ours.is_none() {
        return Err(Failed::Other(format!("{} is not a git repository", repo.display())));
    }
    if tokio::fs::metadata(place).await.is_ok() && common(place.to_path_buf()).await != ours {
        // Made for another clone of the project, or left half made: it is ours to replace.
        tokio::fs::remove_dir_all(place)
            .await
            .map_err(|e| Failed::Other(format!("{}: {e}", place.display())))?;
        bundle::run(git, repo, &["worktree", "prune"]).await?;
    }
    if tokio::fs::metadata(place).await.is_err() {
        if let Some(parent) = place.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Failed::Other(format!("{}: {e}", parent.display())))?;
        }
        let at = place.to_string_lossy().into_owned();
        let add = ["worktree", "add", "--detach", "--force", "--quiet", &at, commit];
        bundle::run(git, repo, &add).await?;
    } else {
        abort_rebase(git, place).await;
        let to = ["checkout", "--detach", "--force", "--quiet", commit];
        bundle::run(git, place, &to).await?;
        bundle::run(git, place, &["clean", "-ffdq"]).await?;
    }
    if tokio::fs::metadata(place.join(".gitmodules")).await.is_ok() {
        let update = ["submodule", "update", "--init", "--recursive", "--quiet"];
        bundle::run(git, place, &update).await?;
    }
    Ok(())
}

/// A rebase left stopped in `place` (a run cut short) undone.
async fn abort_rebase(git: &Path, place: &Path) {
    for dir in ["rebase-merge", "rebase-apply"] {
        let Ok(found) = bundle::run(git, place, &["rev-parse", "--git-path", dir]).await else {
            continue;
        };
        if tokio::fs::metadata(place.join(found.trim())).await.is_ok() {
            if let Err(why) = bundle::run(git, place, &["rebase", "--abort"]).await {
                tracing::warn!(place = %place.display(), %why, "a stopped rebase not undone");
            }
            return;
        }
    }
}

/// Who the rebased commits are committed by: the person's own identity when git has one, and
/// the merge queue's otherwise, so a machine with no `user.name` can still merge.
async fn identity(git: &Path, place: &Path) -> Vec<String> {
    if bundle::run(git, place, &["var", "GIT_COMMITTER_IDENT"]).await.is_ok() {
        return Vec::new();
    }
    ["-c", "user.name=Slopty merge queue", "-c", "user.email=merge-queue@slopty.invalid"]
        .map(str::to_owned)
        .to_vec()
}

/// The checkout that has `reference` checked out, if one has.
async fn checked_out(git: &Path, repo: &Path, reference: &str) -> Result<Option<PathBuf>, Failed> {
    let listed = bundle::run(git, repo, &["worktree", "list", "--porcelain"]).await?;
    let mut tree = None;
    for line in listed.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            tree = Some(PathBuf::from(path));
        } else if line.strip_prefix("branch ") == Some(reference) {
            return Ok(tree);
        }
    }
    Ok(None)
}

/// The commit `what` names in `repo`, in hex.
async fn commit_of(git: &Path, repo: &Path, what: &str) -> Result<String, Failed> {
    let spec = format!("{what}^{{commit}}");
    match bundle::run(git, repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec])
        .await
    {
        Ok(found) => Ok(found.trim().to_owned()),
        Err(_) => Err(Failed::Other(format!("{what} is not a commit in {}", repo.display()))),
    }
}

/// Whether `ancestor` is `of` or comes before it.
async fn is_ancestor(git: &Path, repo: &Path, ancestor: &str, of: &str) -> bool {
    let args = ["merge-base", "--is-ancestor", "--end-of-options", ancestor, of];
    bundle::run(git, repo, &args).await.is_ok()
}

/// A commit as people read it.
fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// The command line a verifier runs as in its terminal.
///
/// It is the person's login shell, as their own terminal runs a command, so their `PATH` and
/// toolchains apply; at a lower priority than anything the person waits on, where `nice` is
/// found.
#[must_use]
pub fn command_line(line: &str) -> Vec<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty());
    let shell = shell.unwrap_or_else(|| "/bin/sh".to_owned());
    let nice = ["/usr/bin/nice", "/bin/nice"].into_iter().find(|p| Path::new(p).is_file());
    let lower = nice.map(|nice| [nice, "-n", "10"].map(str::to_owned).to_vec()).unwrap_or_default();
    lower
        .into_iter()
        .chain([shell, "-l".to_owned(), "-i".to_owned(), "-c".to_owned(), line.to_owned()])
        .collect()
}

#[cfg(test)]
mod tests;
