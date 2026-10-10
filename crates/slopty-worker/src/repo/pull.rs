//! The branch's pull request as the person's own forge command line reads it, its opening and
//! its merge on their word ([`slopty_proto::git::PullStatus`]).
//!
//! The forge is the one the repository's `origin` names ([`crate::repo::forge_of`]): GitHub's
//! pull requests go through `gh`, a GitLab's merge requests through `glab` (`gitlab`). Either
//! runs as the worker's user, signed in as they signed it in, with prompts off; nothing of the
//! sign-in is read or passed. What the forge reports is kept in GitHub's words, a merge
//! request's put in them.

use std::path::Path;

use serde_json::Value;
use slopty_proto::git::{CHECKS_MAX, Forge, GitDone, GitOutcome, PullCheck, PullStatus};

use super::commit::{Programs, REMOTE, run};

mod comments;
mod gitlab;
mod review;

pub use review::Review;

/// What `gh pr view` is asked for.
const FIELDS: &str = "number,url,title,state,isDraft,headRefName,headRefOid,baseRefName,\
                      reviewDecision,mergeable,mergeStateStatus,statusCheckRollup";

/// How gh says the branch has no pull request.
const NONE_FOUND: &str = "no pull requests found";

/// The state of a pull request that merged, in GitHub's words.
const MERGED: &str = "MERGED";

/// The merge methods gh and glab take, by the flag each is.
const METHODS: [&str; 3] = ["merge", "squash", "rebase"];

/// The forge of the repository rooted at `root`: GitHub's when its `origin` names no other.
fn forge(root: &Path) -> Forge {
    crate::repo::forge_of(root).unwrap_or(Forge::GitHub)
}

/// `forge`'s command line among `programs`, or the refusal for a worker without it.
pub(super) fn program(programs: &Programs, forge: Forge) -> Result<&Path, GitOutcome> {
    let (found, why) = match forge {
        Forge::GitHub => (
            programs.gh.as_deref(),
            "gh, GitHub's command line, is not on this worker, so it opens, reads and merges \
             no pull request",
        ),
        Forge::GitLab => (
            programs.glab.as_deref(),
            "glab, GitLab's command line, is not on this worker, so it opens, reads and merges \
             no merge request",
        ),
    };
    found.ok_or_else(|| GitOutcome::Unavailable {
        program: forge.program().to_owned(),
        why: why.to_owned(),
    })
}

/// The pull request for the branch checked out at `root`; none when it has none.
///
/// # Errors
/// The forge's command line is missing, or says something other than its answer.
pub async fn status(programs: &Programs, root: &Path) -> Result<Option<PullStatus>, GitOutcome> {
    let forge = forge(root);
    let program = program(programs, forge)?;
    if forge == Forge::GitLab {
        return gitlab::status(program, root).await;
    }
    match run(program, root, &["pr", "view", "--json", FIELDS], None, REMOTE).await {
        Ok(out) => parse(&out).map(Some),
        Err(GitOutcome::Failed { said }) if said.contains(NONE_FOUND) => Ok(None),
        Err(other) => Err(other),
    }
}

/// The review still open on pull request `number` of the repository at `root`, for its agent
/// to address: its threads not resolved and its reviewers' open words.
///
/// # Errors
/// The forge's command line is missing, or says something other than its answer.
pub async fn comments(
    programs: &Programs,
    root: &Path,
    number: u32,
) -> Result<GitDone, GitOutcome> {
    let forge = forge(root);
    let program = program(programs, forge)?;
    let read = match forge {
        Forge::GitHub => comments::read(program, root, number).await,
        Forge::GitLab => gitlab::comments(program, root, number).await,
    };
    read.map(|c| GitDone::PullComments(Box::new(c)))
}

/// Post the person's `review` of a pull request of the repository at `root`, as one review.
///
/// # Errors
/// The review is out of bounds or says nothing, the forge's command line is missing, the pull
/// request moved past the head reviewed, or the forge refused, in its words.
pub async fn review(
    programs: &Programs,
    root: &Path,
    review: &Review,
) -> Result<GitDone, GitOutcome> {
    let forge = forge(root);
    let program = program(programs, forge)?;
    review::post(forge, program, root, review).await
}

/// [`status`], as a done op.
pub async fn status_done(programs: &Programs, root: &Path) -> Result<GitDone, GitOutcome> {
    Ok(GitDone::PullStatus(status(programs, root).await?.map(Box::new)))
}

/// Open a pull request for the branch checked out: with `title` and `body`, or both from the
/// commits when the title is empty; into `base`, else the repository's default; as a draft.
///
/// # Errors
/// The body is too long, the forge's command line is missing, or it refused, in its words.
pub async fn create(
    programs: &Programs,
    root: &Path,
    (title, body): (&str, &str),
    base: Option<&str>,
    draft: bool,
) -> Result<GitDone, GitOutcome> {
    let forge = forge(root);
    let program = program(programs, forge)?;
    if body.len() > slopty_proto::git::MESSAGE_MAX {
        return Err(GitOutcome::Refused {
            why: format!(
                "a {}'s description is at most {} bytes",
                forge.noun(),
                slopty_proto::git::MESSAGE_MAX
            ),
        });
    }
    let base = base.filter(|b| !b.trim().is_empty());
    let args = match forge {
        Forge::GitHub => {
            let mut args = vec!["pr", "create"];
            if title.trim().is_empty() {
                args.push("--fill");
            } else {
                args.extend(["--title", title.trim(), "--body", body]);
            }
            if let Some(base) = base {
                args.extend(["--base", base]);
            }
            if draft {
                args.push("--draft");
            }
            args
        }
        Forge::GitLab => gitlab::create_args((title, body), base, draft),
    };
    let out = run(program, root, &args, None, REMOTE).await?;
    let url = out.split_whitespace().rev().find(|w| w.starts_with("http"));
    url.map(|u| GitDone::PullRequest { url: u.to_owned() }).ok_or_else(|| GitOutcome::Failed {
        said: format!("{} opened no {} it named: {}", forge.program(), forge.noun(), out.trim()),
    })
}

/// Where the merge queue lands a task's work through the forge ([`land`]).
#[derive(Clone, Copy, Debug)]
pub struct Landing<'a> {
    /// The commit that lands.
    pub head: &'a str,
    /// The branch it goes up as.
    pub branch: &'a str,
    /// The protected branch it lands on.
    pub target: &'a str,
}

/// A refusal of the forge's or git's, in words.
fn words(outcome: GitOutcome) -> String {
    match outcome {
        GitOutcome::Refused { why } | GitOutcome::Unavailable { why, .. } => why,
        GitOutcome::Failed { said } => said,
        GitOutcome::Done(_) => "done".to_owned(),
    }
}

/// The number a pull request's page ends with.
fn number_of(url: &str) -> Option<u32> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// One pull request as `gh pr list --json number,url` lists it.
#[derive(Debug, serde::Deserialize)]
struct Listed {
    number: u32,
    url: String,
}

/// Land `landing.head` on its protected target through the forge of the clone at `root`; the
/// pull request's number and page.
///
/// The commit goes to `origin` as its branch, forced, as the merge queue rebased it. Then the
/// open pull request of that branch into the target is found, else one is opened with `title`
/// and `body`.
///
/// # Errors
/// The branch is no branch's name, git, gh or glab is missing, or git or the forge refused, in
/// their words.
pub async fn land(
    programs: &Programs,
    root: &Path,
    landing: Landing<'_>,
    (title, body): (&str, &str),
) -> Result<(u32, String), String> {
    let Landing { head, branch, target } = landing;
    let named = |b: &str| !b.is_empty() && !b.starts_with('-') && !b.contains("..");
    if !named(branch) || !named(target) {
        return Err(format!("{branch:?} into {target:?} names no branches"));
    }
    let git = programs.git.as_deref().ok_or_else(|| "this worker has no git".to_owned())?;
    let forge = forge(root);
    let program = program(programs, forge).map_err(words)?;
    let refspec = format!("{head}:refs/heads/{branch}");
    let push = ["push", "--quiet", "--force", "--end-of-options", "origin", &refspec];
    run(git, root, &push, None, REMOTE).await.map_err(words)?;
    let (found, open) = match forge {
        Forge::GitHub => {
            let list = [
                "pr",
                "list",
                "--head",
                branch,
                "--base",
                target,
                "--state",
                "open",
                "--json",
                "number,url",
                "--limit",
                "1",
            ];
            let listed = run(program, root, &list, None, REMOTE).await.map_err(words)?;
            let listed: Vec<Listed> = serde_json::from_str(&listed)
                .map_err(|e| format!("gh listed no pull requests: {e}"))?;
            let found = listed.into_iter().next().map(|l| (l.number, l.url));
            let mut open = vec!["pr", "create", "--head", branch, "--base", target];
            open.extend(["--title", title.trim(), "--body", body]);
            (found, open)
        }
        Forge::GitLab => {
            let found = gitlab::open_of(program, root, branch, target).await.map_err(words)?;
            let mut open = gitlab::create_args((title, body), Some(target), false);
            open.extend(["--source-branch", branch]);
            (found, open)
        }
    };
    if let Some(found) = found {
        return Ok(found);
    }
    let out = run(program, root, &open, None, REMOTE).await.map_err(words)?;
    let url = out.split_whitespace().rev().find(|w| w.starts_with("http"));
    url.and_then(|u| Some((number_of(u)?, u.to_owned()))).ok_or_else(|| {
        format!("{} opened no {} it named: {}", forge.program(), forge.noun(), out.trim())
    })
}

/// Merge the branch's pull request by `method`, only while it ends at `head` when given.
///
/// What the forge reads afterwards decides: a command line that fails once the merge went,
/// in the local clean-up `--delete-branch` asks of it, reads as merged, in its own words. gh
/// before 2.99 did so from an agent's worktree, switching the checkout to the base to delete
/// the branch where the clone has the base checked out; since 2.99 it leaves a worktree's
/// branch for the worktree's removal and deletes only the forge's.
///
/// # Errors
/// The method is none the forge takes, its command line is missing, or it refused, in its
/// words.
pub async fn merge(
    programs: &Programs,
    root: &Path,
    method: &str,
    head: Option<&str>,
    delete_branch: bool,
) -> Result<GitDone, GitOutcome> {
    let method = method.trim().to_ascii_lowercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(GitOutcome::Refused {
            why: format!("a merge is by {}; {method:?} is none of them", METHODS.join(", ")),
        });
    }
    let forge = forge(root);
    let program = program(programs, forge)?;
    let head = head.filter(|h| !h.trim().is_empty());
    let args = match forge {
        Forge::GitHub => {
            let flag = format!("--{method}");
            let mut args = vec!["pr".to_owned(), "merge".to_owned(), flag];
            if let Some(head) = head {
                args.extend(["--match-head-commit".to_owned(), head.to_owned()]);
            }
            if delete_branch {
                args.push("--delete-branch".to_owned());
            }
            args
        }
        Forge::GitLab => gitlab::merge_args(&method, head, delete_branch),
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = run(program, root, &args, None, REMOTE).await;
    let pull = status(programs, root).await.ok().flatten().map(Box::new);
    let said = match ran {
        Ok(said) => said,
        Err(GitOutcome::Failed { said }) if pull.as_ref().is_some_and(|p| p.state == MERGED) => {
            said
        }
        Err(failed) => return Err(failed),
    };
    Ok(GitDone::Merged { said: said.trim().to_owned(), pull })
}

/// `gh pr view --json` read.
///
/// # Errors
/// It is not the JSON asked for.
fn parse(out: &str) -> Result<PullStatus, GitOutcome> {
    let doc: Value = serde_json::from_str(out)
        .map_err(|e| GitOutcome::Failed { said: format!("gh answered no pull request: {e}") })?;
    let text = |key: &str| doc.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let number = doc.get("number").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    let Some(number) = number else {
        return Err(GitOutcome::Failed { said: "gh named no pull request number".to_owned() });
    };
    let rollup = doc.get("statusCheckRollup").and_then(Value::as_array);
    let all: Vec<PullCheck> = rollup.into_iter().flatten().filter_map(check).collect();
    let more = u32::try_from(all.len().saturating_sub(CHECKS_MAX)).unwrap_or(u32::MAX);
    Ok(PullStatus {
        forge: Forge::GitHub,
        number,
        url: text("url"),
        title: text("title"),
        state: text("state"),
        draft: doc.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        head: text("headRefName"),
        head_commit: text("headRefOid"),
        base: text("baseRefName"),
        review: text("reviewDecision"),
        mergeable: text("mergeable"),
        merge_state: text("mergeStateStatus"),
        checks: all.into_iter().take(CHECKS_MAX).collect(),
        more_checks: more,
    })
}

/// One entry of the check rollup: a CI job (`CheckRun`) or a commit status another service
/// set (`StatusContext`).
fn check(entry: &Value) -> Option<PullCheck> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
    let name = text("name").or_else(|| text("context"))?.to_owned();
    let state = text("conclusion").or_else(|| text("status")).or_else(|| text("state"));
    Some(PullCheck {
        name,
        workflow: text("workflowName").map(str::to_owned),
        state: state.unwrap_or_default().to_owned(),
        link: text("detailsUrl").or_else(|| text("targetUrl")).map(str::to_owned),
    })
}

#[cfg(test)]
mod tests;
