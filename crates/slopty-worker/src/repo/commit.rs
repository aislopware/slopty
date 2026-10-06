//! The person's commit, push and pull request in a folder's repository ([`slopty_proto::git`]).
//!
//! Each runs the person's own git, or gh for a pull request, in the repository, with their
//! configuration, hooks and credential helpers as they are, and never a prompt: git is told not
//! to ask at a terminal and gh not to prompt, so what would ask fails in its own words. Nothing
//! here reads or writes a credential, and no message is made up: a commit takes the person's,
//! and a pull request with no title is filled by gh from the commits.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use slopty_proto::git::{
    FILES_MAX, GitDone, GitFile, GitOp, GitOutcome, GitStatus, MESSAGE_MAX, PullStatus, SAID_MAX,
};
use tokio::io::AsyncWriteExt as _;

/// How long a status or a commit may take: a commit runs the person's hooks.
const LOCAL: Duration = Duration::from_mins(5);
/// How long a push or a pull request may take.
pub(super) const REMOTE: Duration = Duration::from_mins(10);

/// The person's programs an op runs: git, and gh for a pull request.
#[derive(Clone, Debug, Default)]
pub struct Programs {
    /// git, when the worker has it.
    pub git: Option<PathBuf>,
    /// gh, when the worker has it.
    pub gh: Option<PathBuf>,
}

impl Programs {
    /// The ones this worker has: git as everything else finds it ([`crate::changes::git`]),
    /// gh on `PATH` or where Homebrew and the system put it ([`super::checks::find`]).
    #[must_use]
    pub fn here() -> Self {
        Self {
            git: crate::changes::git().map(Path::to_path_buf),
            gh: super::checks::find("gh", std::env::var_os("PATH").as_deref()),
        }
    }
}

/// Do `op` in the repository holding `repo` (absolute, or `~/…`), with `programs`.
/// `terminals` holds the directory of every terminal live on the worker, which a worktree's
/// removal never pulls out from under.
pub async fn apply(
    programs: &Programs,
    repo: &str,
    op: GitOp,
    terminals: &[PathBuf],
) -> GitOutcome {
    let gh = programs.gh.as_deref();
    let Some(git) = programs.git.as_deref() else {
        return GitOutcome::Unavailable {
            program: "git".to_owned(),
            why: "git is not on this worker".to_owned(),
        };
    };
    let folder = crate::file::expand_home(Path::new(repo));
    let root = match run(git, &folder, &["rev-parse", "--show-toplevel"], None, LOCAL).await {
        Ok(out) => PathBuf::from(out.trim()),
        Err(_) => {
            return GitOutcome::Refused { why: format!("{repo} is in no git repository") };
        }
    };
    let done = match op {
        GitOp::Status => status(git, &root).await.map(|s| GitDone::Status(Box::new(s))),
        GitOp::Commit { paths, message } => commit(git, &root, &paths, &message).await,
        GitOp::Push => push(git, gh, &root).await,
        GitOp::PullRequest { title, body, base, draft } => {
            pull_request(gh, &root, (&title, &body), base.as_deref(), draft).await
        }
        GitOp::PullStatus => super::pull::status_done(gh, &root).await,
        GitOp::Merge { method, head, delete_branch } => {
            super::pull::merge(gh, &root, &method, head.as_deref(), delete_branch).await
        }
        GitOp::Changes { against } => super::snapshot::working_tree(git, &root, against)
            .await
            .map(|review| GitDone::Changes(Box::new(review)))
            .map_err(|failed| GitOutcome::Failed { said: failed.0 }),
        GitOp::Branches => {
            super::branches::branches(git, &root).await.map(|b| GitDone::Branches(Box::new(b)))
        }
        GitOp::RemoveWorktree => {
            use super::worktrees::{Failed, Removed, free};
            match free(git, gh, &root, terminals).await {
                Ok(Removed { branch, branch_removed }) => {
                    Ok(GitDone::WorktreeRemoved { branch, branch_removed })
                }
                Err(Failed::Other(said)) => Err(GitOutcome::Failed { said }),
                Err(refused) => Err(GitOutcome::Refused { why: refused.to_string() }),
            }
        }
    };
    done.map_or_else(|o| o, GitOutcome::Done)
}

/// The repository at `root` as `git status --porcelain=v2` sees it.
async fn status(git: &Path, root: &Path) -> Result<GitStatus, GitOutcome> {
    let args = ["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"];
    let out = run(git, root, &args, None, LOCAL).await?;
    Ok(parse_status(&root.to_string_lossy(), &out))
}

/// `git status --porcelain=v2 --branch -z` read: its branch headers, then one record per
/// changed file, a rename's or copy's original path in the record after it.
fn parse_status(root: &str, out: &str) -> GitStatus {
    let mut status = GitStatus {
        root: root.to_owned(),
        branch: None,
        head: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        files: Vec::new(),
        more: 0,
    };
    let mut records = out.split('\0').filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let file = if let Some(header) = record.strip_prefix("# ") {
            header_into(&mut status, header);
            None
        } else if let Some(rest) = record.strip_prefix("1 ") {
            fields(rest, 7).map(|(xy, path)| GitFile { path, from: None, xy })
        } else if let Some(rest) = record.strip_prefix("2 ") {
            let from = records.next().map(str::to_owned);
            fields(rest, 8).map(|(xy, path)| GitFile { path, from, xy })
        } else if let Some(rest) = record.strip_prefix("u ") {
            fields(rest, 9).map(|(xy, path)| GitFile { path, from: None, xy })
        } else if let Some(path) = record.strip_prefix("? ") {
            Some(GitFile { path: path.to_owned(), from: None, xy: "??".to_owned() })
        } else {
            record.strip_prefix("! ").map(|path| GitFile {
                path: path.to_owned(),
                from: None,
                xy: "!!".to_owned(),
            })
        };
        let Some(file) = file else { continue };
        if status.files.len() < FILES_MAX {
            status.files.push(file);
        } else {
            status.more = status.more.saturating_add(1);
        }
    }
    status
}

/// A `# branch.…` header taken into `status`.
fn header_into(status: &mut GitStatus, header: &str) {
    let Some((key, value)) = header.split_once(' ') else { return };
    match key {
        "branch.oid" => status.head = Some(value.to_owned()).filter(|v| v != "(initial)"),
        "branch.head" => status.branch = Some(value.to_owned()).filter(|v| v != "(detached)"),
        "branch.upstream" => status.upstream = Some(value.to_owned()),
        "branch.ab" => {
            let mut counts = value.split(' ').map(|n| n.trim_start_matches(['+', '-']).parse());
            status.ahead = counts.next().and_then(Result::ok).unwrap_or(0);
            status.behind = counts.next().and_then(Result::ok).unwrap_or(0);
        }
        _ => {}
    }
}

/// A record's two status letters, and its path after `skip` more fields.
fn fields(rest: &str, skip: usize) -> Option<(String, String)> {
    let mut parts = rest.splitn(skip.saturating_add(1), ' ');
    let xy = parts.next()?.to_owned();
    let path = parts.nth(skip.saturating_sub(1))?.to_owned();
    Some((xy, path))
}

/// Commit `paths`, and nothing else staged, with `message`.
async fn commit(
    git: &Path,
    root: &Path,
    paths: &[String],
    message: &str,
) -> Result<GitDone, GitOutcome> {
    let refuse = |why: &str| GitOutcome::Refused { why: why.to_owned() };
    if paths.is_empty() {
        return Err(refuse("choose the files to commit"));
    }
    if message.trim().is_empty() {
        return Err(refuse("a commit takes your message; none is made up for you"));
    }
    if message.len() > MESSAGE_MAX {
        return Err(refuse(&format!("a commit message is at most {MESSAGE_MAX} bytes")));
    }
    if let Some(bad) = paths.iter().find(|p| !inside(p)) {
        return Err(refuse(&format!("{bad} is not a path inside the repository")));
    }
    let mut add = vec!["add", "--all", "--"];
    add.extend(paths.iter().map(String::as_str));
    run(git, root, &add, None, LOCAL).await?;
    let mut commit = vec!["commit", "--file=-", "--only", "--"];
    commit.extend(paths.iter().map(String::as_str));
    run(git, root, &commit, Some(message), LOCAL).await?;
    let head = run(git, root, &["rev-parse", "HEAD"], None, LOCAL).await?.trim().to_owned();
    let branch = current_branch(git, root).await;
    let changed =
        run(git, root, &["show", "--name-only", "--format=", "HEAD"], None, LOCAL).await?;
    let files = u32::try_from(changed.lines().filter(|l| !l.is_empty()).count()).unwrap_or(0);
    Ok(GitDone::Committed { commit: head, branch, files })
}

/// Whether `path` names something inside the repository: relative, never climbing out.
fn inside(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// The branch checked out at `root`; none on a detached `HEAD`.
async fn current_branch(git: &Path, root: &Path) -> Option<String> {
    let out = run(git, root, &["symbolic-ref", "--quiet", "--short", "HEAD"], None, LOCAL).await;
    out.ok().map(|b| b.trim().to_owned()).filter(|b| !b.is_empty())
}

/// Push the branch checked out: to its upstream, or setting one on the repository's only
/// remote, else `origin`.
async fn push(git: &Path, gh: Option<&Path>, root: &Path) -> Result<GitDone, GitOutcome> {
    let Some(branch) = current_branch(git, root).await else {
        return Err(GitOutcome::Refused {
            why: "HEAD is detached: check out a branch to push".to_owned(),
        });
    };
    let upstream = format!("{branch}@{{upstream}}");
    let tracked = run(
        git,
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", &upstream],
        None,
        LOCAL,
    )
    .await
    .ok()
    .map(|u| u.trim().to_owned())
    .filter(|u| !u.is_empty());
    if let Some(tracked) = tracked {
        run(git, root, &["push", "--porcelain"], None, REMOTE).await?;
        let remote = tracked.split_once('/').map_or(tracked.as_str(), |(r, _)| r).to_owned();
        let pull = pushed_pull(gh, root).await;
        return Ok(GitDone::Pushed { remote, branch, upstream_set: false, pull });
    }
    let remotes = run(git, root, &["remote"], None, LOCAL).await?;
    let remotes: Vec<&str> = remotes.lines().map(str::trim).filter(|r| !r.is_empty()).collect();
    let remote = match &*remotes {
        [] => {
            return Err(GitOutcome::Refused {
                why: "this repository has no remote to push to".to_owned(),
            });
        }
        [only] => (*only).to_owned(),
        _ if remotes.contains(&"origin") => "origin".to_owned(),
        _ => {
            return Err(GitOutcome::Refused {
                why: format!(
                    "{branch} has no upstream, and the remotes are {}: push it once with git \
                     push --set-upstream to the one you mean",
                    remotes.join(", ")
                ),
            });
        }
    };
    run(git, root, &["push", "--porcelain", "--set-upstream", &remote, &branch], None, REMOTE)
        .await?;
    let pull = pushed_pull(gh, root).await;
    Ok(GitDone::Pushed { remote, branch, upstream_set: true, pull })
}

/// The branch's pull request after a push, so its checks read as started: none when gh is
/// missing, says the branch has none, or fails, since the push itself went.
async fn pushed_pull(gh: Option<&Path>, root: &Path) -> Option<Box<PullStatus>> {
    super::pull::status(gh, root).await.ok().flatten().map(Box::new)
}

/// Open a pull request for the branch checked out with the person's own gh.
async fn pull_request(
    gh: Option<&Path>,
    root: &Path,
    (title, body): (&str, &str),
    base: Option<&str>,
    draft: bool,
) -> Result<GitDone, GitOutcome> {
    let gh = super::pull::gh(gh)?;
    if body.len() > MESSAGE_MAX {
        return Err(GitOutcome::Refused {
            why: format!("a pull request's description is at most {MESSAGE_MAX} bytes"),
        });
    }
    let mut args = vec!["pr", "create"];
    if title.trim().is_empty() {
        args.push("--fill");
    } else {
        args.extend(["--title", title.trim(), "--body", body]);
    }
    if let Some(base) = base.filter(|b| !b.trim().is_empty()) {
        args.extend(["--base", base]);
    }
    if draft {
        args.push("--draft");
    }
    let out = run(gh, root, &args, None, REMOTE).await?;
    let url = out.lines().rev().map(str::trim).find(|l| l.starts_with("http"));
    url.map(|u| GitDone::PullRequest { url: u.to_owned() }).ok_or_else(|| GitOutcome::Failed {
        said: format!("gh opened no pull request it named: {}", out.trim()),
    })
}

/// `program args…` in `root`, with `input` on its stdin, within `within`: its stdout when it
/// succeeds, else what it said, its end kept.
pub(super) async fn run(
    program: &Path,
    root: &Path,
    args: &[&str],
    input: Option<&str>,
    within: Duration,
) -> Result<String, GitOutcome> {
    let mut command = tokio::process::Command::new(program);
    command
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_EDITOR", "true")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let name = program.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let failed = |said: String| GitOutcome::Failed { said };
    let mut child = command.spawn().map_err(|e| failed(format!("{name} did not start: {e}")))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(text.as_bytes()).await.map_err(|e| failed(format!("{name}: {e}")))?;
    }
    let out = tokio::time::timeout(within, child.wait_with_output())
        .await
        .map_err(|_elapsed| {
            failed(format!("{name} {} took too long", args.first().unwrap_or(&"")))
        })?
        .map_err(|e| failed(format!("{name}: {e}")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let (stderr, stdout) =
        (String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
    // Both: `git push --porcelain` says only that it failed on stderr, and which ref was
    // refused and why on stdout.
    let said: Vec<&str> =
        [stderr.trim(), stdout.trim()].into_iter().filter(|s| !s.is_empty()).collect();
    let said = if said.is_empty() {
        format!("{name} {} failed", args.first().unwrap_or(&""))
    } else {
        tail(&said.join("\n"), SAID_MAX)
    };
    Err(failed(said))
}

/// The last `max` bytes of `text`, cut at a character.
fn tail(text: &str, max: usize) -> String {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    text.get(start..).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests;
