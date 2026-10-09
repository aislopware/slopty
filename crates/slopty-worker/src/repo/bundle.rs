//! A task's branch carried from the worker it ran on to the orchestrator's clone, as a git
//! bundle the server relays between the two workers' links.
//!
//! The worker the branch is on bundles the commits it has beyond where it left the target
//! branch ([`bundle_branch`]); the server reads that file in parts and uploads it into the
//! other worker's bundle place ([`slopty_proto::orchestration::BUNDLES`]), which fetches the
//! branch from it under the same name ([`fetch_bundle`]). Neither worker needs a credential
//! for the other or for the forge, and nothing is pushed anywhere. A bundle is removed once
//! fetched, and any left an hour is swept ([`sweep`]).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// How long one git run of a bundle may take.
pub const TIMEOUT: Duration = Duration::from_mins(10);
/// How long a bundle is kept unfetched before it is swept.
pub const KEPT: Duration = Duration::from_hours(1);

/// A branch bundled ([`bundle_branch`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bundled {
    /// The file.
    pub path: PathBuf,
    /// Its name in the bundle place.
    pub name: String,
    /// Its size.
    pub size: u64,
    /// Its BLAKE3 digest.
    pub digest: [u8; 32],
    /// The commit the branch is at.
    pub head: String,
    /// The commit it starts after, when it holds only the branch's own.
    pub base: Option<String>,
}

/// Why a bundle could not be made or fetched.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Failed {
    /// The receiving repository lacks a commit the bundle starts after: send the whole
    /// branch.
    Prerequisites(String),
    /// The branch has no commit beyond where it left the target: there is nothing to bundle,
    /// and a clone of the same forge has it all already.
    NothingNew(String),
    /// Anything else, in words.
    Other(String),
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prerequisites(why) | Self::NothingNew(why) | Self::Other(why) => f.write_str(why),
        }
    }
}

/// Bundle `branch` of the repository at `repo` into `dir`.
///
/// The bundle holds the commits beyond its fork point from `target` (`origin/<target>`, else
/// `<target>`), or the whole branch when `target` is `None` or neither is there.
///
/// # Errors
/// [`Failed::NothingNew`] when the branch has no commit beyond `target`; otherwise a branch
/// that is not a branch here, or a git that failed.
pub async fn bundle_branch(
    git: &Path,
    repo: &Path,
    branch: &str,
    target: Option<&str>,
    dir: &Path,
) -> Result<Bundled, Failed> {
    let reference = branch_ref(branch)?;
    let head = run(
        git,
        repo,
        &["rev-parse", "--verify", "--end-of-options", &format!("{reference}^{{commit}}")],
    )
    .await?
    .trim()
    .to_owned();
    let mut base = None;
    if let Some(target) = target {
        for candidate in [format!("refs/remotes/origin/{target}"), format!("refs/heads/{target}")] {
            if let Ok(fork) =
                run(git, repo, &["merge-base", "--end-of-options", &head, &candidate]).await
            {
                base = Some(fork.trim().to_owned());
                break;
            }
        }
    }
    if base.as_deref() == Some(head.as_str()) {
        let target = target.unwrap_or_default();
        return Err(Failed::NothingNew(format!("{branch} has no commit beyond {target}")));
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| Failed::Other(format!("{}: {e}", dir.display())))?;
    sweep(dir).await;
    let short = head.get(..12).unwrap_or(&head);
    let name = format!("{}-{short}.bundle", file_safe(branch));
    let path = dir.join(&name);
    let path_text = path.to_string_lossy().into_owned();
    let mut args = vec!["bundle", "create", "--quiet", path_text.as_str(), reference.as_str()];
    let not_base = base.as_ref().map(|b| format!("^{b}"));
    args.extend(not_base.as_deref());
    run(git, repo, &args).await?;
    let (size, digest) = digest_of(&path).await.map_err(Failed::Other)?;
    Ok(Bundled { path, name, size, digest, head, base })
}

/// What a [`fetch_bundle`] takes: which bundle, the branch in it, and where it lands.
#[derive(Clone, Copy, Debug)]
pub struct Fetch<'a> {
    /// The bundle's name in the bundle place.
    pub name: &'a str,
    /// The branch it holds.
    pub branch: &'a str,
    /// The branch it lands as, set to it whatever it held: a name only the server gives
    /// (`slopty/<project>/<task>`), so no branch of the person's is ever moved.
    pub into: &'a str,
    /// The commit `branch` is at, which the bundle must hold.
    pub head: &'a str,
}

/// Fetch `want.branch` from the bundle `want.name` in `dir` into the repository at `repo`, as
/// `want.into`, at `want.head`; the bundle goes after, fetched or not.
///
/// A repository that lacks the commit the bundle starts after fetches its `origin` first,
/// with its own credentials: the fork point is the target branch as the other clone fetched
/// it, often later than this one.
///
/// # Errors
/// [`Failed::Prerequisites`] when the repository still lacks the commit the bundle starts
/// after; otherwise a bundle not there or not a bundle, `into` checked out here, a head that
/// differs.
pub async fn fetch_bundle(
    git: &Path,
    repo: &Path,
    dir: &Path,
    want: Fetch<'_>,
) -> Result<String, Failed> {
    let name = want.name;
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(Failed::Other(format!("{name} is not a bundle's name")));
    }
    let path = dir.join(name);
    let fetched = fetch_from(git, repo, &path, want).await;
    if let Err(e) = tokio::fs::remove_file(&path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "a fetched bundle not removed");
    }
    fetched
}

async fn fetch_from(
    git: &Path,
    repo: &Path,
    path: &Path,
    want: Fetch<'_>,
) -> Result<String, Failed> {
    let (from, into) = (branch_ref(want.branch)?, branch_ref(want.into)?);
    let bundle = path.to_string_lossy().into_owned();
    if let Err(Failed::Prerequisites(_)) = verify(git, repo, &bundle).await {
        let has_origin = run(git, repo, &["remote", "get-url", "origin"]).await.is_ok();
        if has_origin && let Err(why) = run(git, repo, &["fetch", "--quiet", "origin"]).await {
            tracing::info!(repo = %repo.display(), %why, "origin not fetched for a bundle");
        }
    }
    verify(git, repo, &bundle).await?;
    let refspec = format!("+{from}:{into}");
    run(git, repo, &["fetch", "--quiet", "--no-tags", "--end-of-options", &bundle, &refspec])
        .await?;
    let now = run(
        git,
        repo,
        &["rev-parse", "--verify", "--end-of-options", &format!("{into}^{{commit}}")],
    )
    .await?
    .trim()
    .to_owned();
    if now != want.head {
        return Err(Failed::Other(format!("{} came at {now}, not {}", want.into, want.head)));
    }
    Ok(now)
}

/// `git bundle verify`: whether the repository at `repo` has every commit the bundle starts
/// after.
async fn verify(git: &Path, repo: &Path, bundle: &str) -> Result<(), Failed> {
    // Not `--quiet`: it silences the very list of missing commits that tells the cases apart.
    match run(git, repo, &["bundle", "verify", bundle]).await {
        Ok(_) => Ok(()),
        Err(why) => {
            let text = why.to_string();
            Err(if text.contains("prerequisite") {
                Failed::Prerequisites(text)
            } else {
                Failed::Other(text)
            })
        }
    }
}

/// `refs/heads/<branch>`, for a branch name git takes as one.
pub(super) fn branch_ref(branch: &str) -> Result<String, Failed> {
    let fine = !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains("..")
        && !branch.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    if fine {
        Ok(format!("refs/heads/{branch}"))
    } else {
        Err(Failed::Other(format!("{branch:?} is not a branch name")))
    }
}

/// A branch name as a file name: its slashes as dashes.
fn file_safe(branch: &str) -> String {
    branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' })
        .collect()
}

/// Remove the bundles in `dir` kept longer than [`KEPT`].
pub async fn sweep(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else { return };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let old = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > KEPT);
        if !old {
            continue;
        }
        let path = entry.path();
        if let Err(e) = tokio::fs::remove_file(&path).await {
            tracing::debug!(path = %path.display(), error = %e, "an old bundle not swept");
        }
    }
}

async fn digest_of(path: &Path) -> Result<(u64, [u8; 32]), String> {
    let path = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || -> std::io::Result<(u64, [u8; 32])> {
        let mut hasher = blake3::Hasher::new();
        let mut file = std::fs::File::open(&path)?;
        let size = std::io::copy(&mut file, &mut hasher)?;
        Ok((size, *hasher.finalize().as_bytes()))
    });
    read.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}

/// `git -C repo args…`, asking nothing, within [`TIMEOUT`]: its output, or what it said.
pub(super) async fn run(git: &Path, repo: &Path, args: &[&str]) -> Result<String, Failed> {
    run_within(git, repo, args, TIMEOUT).await
}

/// [`run`], given up on (and the git killed) after `wait`.
pub(super) async fn run_within(
    git: &Path,
    repo: &Path,
    args: &[&str],
    wait: Duration,
) -> Result<String, Failed> {
    let ran = tokio::process::Command::new(git)
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(wait, ran)
        .await
        .map_err(|_elapsed| {
            Failed::Other(format!("git {} took too long", args.first().unwrap_or(&"")))
        })?
        .map_err(|e| Failed::Other(format!("git did not start: {e}")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let line = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("git failed");
    let all = said.trim();
    // What the remote said is why a push was refused (a protected branch, a hook): it goes
    // before git's own last line, which says only that it was.
    let remote: Vec<&str> = said
        .lines()
        .filter_map(|l| l.trim().strip_prefix("remote:"))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Err(Failed::Other(if all.contains("prerequisite") {
        all.to_owned()
    } else if remote.is_empty() {
        line.trim().to_owned()
    } else {
        format!("{}: {}", remote.join(" "), line.trim())
    }))
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

    /// Two clones of one repository on two machines, the second made after the target moved
    /// on: the branch made in a worktree of it reaches the first through a bundle of its own
    /// commits alone, as the name it is given, at its commit, once the first fetched its origin
    /// for the fork point; the bundle goes, and no branch of the person's moves. A branch with
    /// nothing new says so; a receiver with no origin to fetch the fork point from asks for the
    /// whole branch, which then fetches.
    #[tokio::test]
    async fn a_branch_reaches_another_clone_through_a_bundle_of_its_own_commits() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).expect("mkdir");
        git_in(&origin, &["init", "-q", "-b", "main"]);
        for n in 0..3 {
            git_in(&origin, &["commit", "-q", "--allow-empty", "-m", &format!("c{n}")]);
        }
        git_in(tmp.path(), &["clone", "-q", "origin", "studio"]);
        git_in(&origin, &["commit", "-q", "--allow-empty", "-m", "c3"]);
        git_in(tmp.path(), &["clone", "-q", "origin", "linux"]);
        let linux = tmp.path().join("linux");
        let tree = tmp.path().join("linux/.claude/worktrees/slopty-demo-1");
        let branch = "worktree-slopty-demo-1";
        let tree_text = tree.to_string_lossy().into_owned();
        git_in(&linux, &["worktree", "add", "-q", "-b", branch, &tree_text, "origin/main"]);
        std::fs::write(tree.join("work.txt"), "done").expect("write");
        git_in(&tree, &["add", "."]);
        git_in(&tree, &["commit", "-q", "-m", "the work"]);
        let head = git_in(&tree, &["rev-parse", "HEAD"]);

        let sent = tmp.path().join("sent");
        let made = bundle_branch(git, &tree, branch, Some("main"), &sent).await.expect("bundled");
        assert_eq!(made.head, head);
        assert_eq!(
            made.base.as_deref(),
            Some(git_in(&linux, &["rev-parse", "origin/main"]).as_str())
        );
        assert!(made.size > 0 && made.path.exists());

        let got = tmp.path().join("got");
        std::fs::create_dir_all(&got).expect("mkdir");
        std::fs::copy(&made.path, got.join(&made.name)).expect("copy");
        let studio = tmp.path().join("studio");
        let main_before = git_in(&studio, &["rev-parse", "main"]);
        let into = "slopty/demo/1";
        let want = Fetch { name: &made.name, branch, into, head: &head };
        assert_eq!(fetch_bundle(git, &studio, &got, want).await, Ok(head.clone()));
        assert_eq!(git_in(&studio, &["rev-parse", into]), head);
        assert_eq!(git_in(&studio, &["rev-parse", "main"]), main_before, "the person's own");
        assert!(!got.join(&made.name).exists(), "the bundle goes once fetched");

        let nothing = bundle_branch(git, &linux, "main", Some("main"), &sent).await;
        assert!(
            matches!(nothing, Err(Failed::NothingNew(why)) if why.contains("no commit beyond"))
        );

        // A repository with no origin that has the fork point: the bundle of the branch's own
        // commits cannot go in, and the whole branch does.
        let stranger = tmp.path().join("stranger");
        std::fs::create_dir_all(&stranger).expect("mkdir");
        git_in(&stranger, &["init", "-q"]);
        std::fs::copy(&made.path, got.join(&made.name)).expect("copy");
        let lacking = fetch_bundle(git, &stranger, &got, want).await;
        assert!(matches!(lacking, Err(Failed::Prerequisites(_))), "{lacking:?}");
        let whole = bundle_branch(git, &tree, branch, None, &sent).await.expect("bundled");
        assert_eq!(whole.base, None);
        std::fs::copy(&whole.path, got.join(&whole.name)).expect("copy");
        let want = Fetch { name: &whole.name, ..want };
        assert_eq!(fetch_bundle(git, &stranger, &got, want).await, Ok(head));
    }

    /// The other way, for a task the merge queue gives back: the orchestrator's `main`, with
    /// commits the forge never saw, reaches the task's clone on another machine as a branch
    /// only the server names, from its commits beyond the forge's `main` alone. The agent's
    /// work then rebases onto it there. A `main` the forge has already is nothing new to carry.
    #[tokio::test]
    async fn the_target_reaches_a_task_s_clone_the_same_way_with_what_the_forge_lacks() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).expect("mkdir");
        git_in(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("a.txt"), "one\n").expect("write");
        git_in(&origin, &["add", "."]);
        git_in(&origin, &["commit", "-q", "-m", "first"]);
        git_in(tmp.path(), &["clone", "-q", "origin", "studio"]);
        git_in(tmp.path(), &["clone", "-q", "origin", "linux"]);
        let (studio, linux) = (tmp.path().join("studio"), tmp.path().join("linux"));
        let sent = tmp.path().join("sent");
        let nothing = bundle_branch(git, &studio, "main", Some("main"), &sent).await;
        assert!(matches!(nothing, Err(Failed::NothingNew(_))), "the forge has it: {nothing:?}");

        std::fs::write(studio.join("b.txt"), "merged\n").expect("write");
        git_in(&studio, &["add", "."]);
        git_in(&studio, &["commit", "-q", "-m", "merged by the queue"]);
        let target = git_in(&studio, &["rev-parse", "main"]);
        git_in(&linux, &["switch", "-q", "-c", "task-1"]);
        std::fs::write(linux.join("c.txt"), "the task's\n").expect("write");
        git_in(&linux, &["add", "."]);
        git_in(&linux, &["commit", "-q", "-m", "the work"]);

        let made = bundle_branch(git, &studio, "main", Some("main"), &sent).await.expect("bundled");
        assert_eq!(made.head, target);
        assert_eq!(made.base, Some(git_in(&studio, &["rev-parse", "origin/main"])));
        let got = tmp.path().join("got");
        std::fs::create_dir_all(&got).expect("mkdir");
        std::fs::copy(&made.path, got.join(&made.name)).expect("copy");
        let into = "slopty/demo/target";
        let want = Fetch { name: &made.name, branch: "main", into, head: &target };
        assert_eq!(fetch_bundle(git, &linux, &got, want).await, Ok(target.clone()));
        assert_eq!(
            git_in(&linux, &["rev-parse", "main"]),
            git_in(&linux, &["rev-parse", "origin/main"])
        );
        git_in(&linux, &["rebase", "-q", into]);
        assert_eq!(git_in(&linux, &["rev-parse", "HEAD~1"]), target, "the work on top of it");
    }

    #[test]
    fn only_a_branch_name_is_taken() {
        assert_eq!(branch_ref("feature/rows"), Ok("refs/heads/feature/rows".to_owned()));
        for bad in ["", "-x", "a..b", "a b", "a:b", "a~1"] {
            assert!(branch_ref(bad).is_err(), "{bad:?}");
        }
        assert_eq!(file_safe("feature/rows"), "feature-rows");
    }
}
