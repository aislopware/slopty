//! The branch's pull request as the person's own gh reads it, and its merge on their word
//! ([`slopty_proto::git::PullStatus`]).
//!
//! gh runs as the worker's user, signed in as they signed it in, with prompts off; nothing of
//! the sign-in is read or passed. What the forge reports is kept in its own words.

use std::path::Path;

use serde_json::Value;
use slopty_proto::git::{CHECKS_MAX, GitDone, GitOutcome, PullCheck, PullStatus};

use super::commit::{REMOTE, run};

/// What `gh pr view` is asked for.
const FIELDS: &str = "number,url,title,state,isDraft,headRefName,headRefOid,baseRefName,\
                      reviewDecision,mergeable,mergeStateStatus,statusCheckRollup";

/// How gh says the branch has no pull request.
const NONE_FOUND: &str = "no pull requests found";

/// The merge methods gh takes, by the flag each is.
const METHODS: [&str; 3] = ["merge", "squash", "rebase"];

/// `gh`, or the refusal for a worker without it.
pub(super) fn gh(gh: Option<&Path>) -> Result<&Path, GitOutcome> {
    gh.ok_or_else(|| GitOutcome::Unavailable {
        program: "gh".to_owned(),
        why: "gh, GitHub's command line, is not on this worker, so it opens, reads and merges \
              no pull request"
            .to_owned(),
    })
}

/// The pull request for the branch checked out at `root`; none when it has none.
///
/// # Errors
/// gh is missing, or says something other than its answer.
pub async fn status(gh: Option<&Path>, root: &Path) -> Result<Option<PullStatus>, GitOutcome> {
    let gh = self::gh(gh)?;
    match run(gh, root, &["pr", "view", "--json", FIELDS], None, REMOTE).await {
        Ok(out) => parse(&out).map(Some),
        Err(GitOutcome::Failed { said }) if said.contains(NONE_FOUND) => Ok(None),
        Err(other) => Err(other),
    }
}

/// [`status`], as a done op.
pub async fn status_done(gh: Option<&Path>, root: &Path) -> Result<GitDone, GitOutcome> {
    Ok(GitDone::PullStatus(status(gh, root).await?.map(Box::new)))
}

/// Merge the branch's pull request by `method`, only while it ends at `head` when given.
///
/// # Errors
/// The method is none gh takes, gh is missing, or gh refused, in its words.
pub async fn merge(
    gh: Option<&Path>,
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
    let program = self::gh(gh)?;
    let flag = format!("--{method}");
    let mut args = vec!["pr", "merge", flag.as_str()];
    if let Some(head) = head.filter(|h| !h.trim().is_empty()) {
        args.extend(["--match-head-commit", head]);
    }
    if delete_branch {
        args.push("--delete-branch");
    }
    let said = run(program, root, &args, None, REMOTE).await?;
    let pull = status(gh, root).await.ok().flatten().map(Box::new);
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
