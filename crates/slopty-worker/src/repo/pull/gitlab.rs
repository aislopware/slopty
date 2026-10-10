//! A GitLab merge request through the person's own `glab`, in the words a GitHub pull request
//! is read in ([`PullStatus`]), so everything that reads one reads the other.
//!
//! - **Read.** `glab mr view --output json` finds the open merge request of the branch checked out;
//!   with none open, the branch's merged one (`glab mr list --merged`) is read, as gh's view finds
//!   a merged pull request too. Its head pipeline's jobs are its checks, read with `glab api` under
//!   the project glab resolves from the checkout; when they cannot be read, the pipeline stands as
//!   one check.
//! - **Opened and merged** with `glab mr create` and `glab mr merge`, as gh's are, and never left
//!   to merge on its own later: a merge is now, on the person's word.
//! - **Its review still open** is each discussion that can be resolved and is not, read with `glab
//!   api` as the pipeline's jobs are.

use std::path::Path;

use serde::Deserialize;
use slopty_proto::git::{
    CHECKS_MAX, Forge, GitOutcome, PullCheck, PullComments, PullStatus, PullThread,
};

use super::{REMOTE, run};

/// How glab says the branch has no merge request ("no open merge request available for …").
const NONE_FOUND: &str = "merge request available for";

/// The most jobs of a pipeline read: past it the rest are not asked for.
const JOBS_PAGE: &str = "100";

/// What `glab mr view --output json` prints, as far as it is read: the API's merge request.
#[derive(Debug, Deserialize)]
struct Request {
    iid: u32,
    #[serde(default)]
    web_url: String,
    #[serde(default)]
    title: String,
    /// `opened`, `closed`, `locked`, `merged`.
    #[serde(default)]
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    source_branch: String,
    /// The commit the source branch ends at.
    #[serde(default)]
    sha: String,
    #[serde(default)]
    target_branch: String,
    /// What stands between it and a merge: `mergeable`, `ci_still_running`, `not_approved`, …
    #[serde(default)]
    detailed_merge_status: String,
    /// `can_be_merged`, `cannot_be_merged`, `checking`, `unchecked`.
    #[serde(default)]
    merge_status: String,
    #[serde(default)]
    has_conflicts: bool,
    head_pipeline: Option<Pipeline>,
}

#[derive(Debug, Deserialize)]
struct Pipeline {
    id: u64,
    /// `created`, `pending`, `running`, `success`, `failed`, `canceled`, `skipped`, `manual`, …
    #[serde(default)]
    status: String,
    web_url: Option<String>,
}

/// A pipeline's job, as `GET projects/:id/pipelines/:pipeline/jobs` lists it.
#[derive(Debug, Deserialize)]
struct Job {
    name: String,
    #[serde(default)]
    stage: String,
    #[serde(default)]
    status: String,
    /// A failure that does not fail the pipeline.
    #[serde(default)]
    allow_failure: bool,
    web_url: Option<String>,
}

/// One merge request as `glab mr list --output json` lists it, as far as its number.
#[derive(Debug, Deserialize)]
struct Listed {
    iid: u32,
}

/// One open merge request as `glab mr list --output json` lists it, with its page.
#[derive(Debug, Deserialize)]
struct Open {
    iid: u32,
    #[serde(default)]
    web_url: String,
}

/// The open merge request of `branch` into `target`, as its number and page; none when there
/// is none.
///
/// # Errors
/// glab says something other than its answer.
pub(super) async fn open_of(
    glab: &Path,
    root: &Path,
    branch: &str,
    target: &str,
) -> Result<Option<(u32, String)>, GitOutcome> {
    let list = [
        "mr",
        "list",
        "--source-branch",
        branch,
        "--target-branch",
        target,
        "--per-page",
        "1",
        "--output",
        "json",
    ];
    let listed = run(glab, root, &list, None, REMOTE).await?;
    let listed: Vec<Open> = serde_json::from_str(&listed)
        .map_err(|e| GitOutcome::Failed { said: format!("glab listed no merge requests: {e}") })?;
    Ok(listed.into_iter().next().map(|o| (o.iid, o.web_url)))
}

/// The merge request of the branch checked out at `root`, read with `glab`; none when the
/// branch has none, open or merged.
///
/// # Errors
/// glab says something other than its answer.
pub(super) async fn status(glab: &Path, root: &Path) -> Result<Option<PullStatus>, GitOutcome> {
    let view = match run(glab, root, &["mr", "view", "--output", "json"], None, REMOTE).await {
        Ok(view) => view,
        Err(GitOutcome::Failed { said }) if said.contains(NONE_FOUND) => {
            let Some(branch) = crate::repo::branch_of(root) else { return Ok(None) };
            let list = [
                "mr",
                "list",
                "--merged",
                "--source-branch",
                &branch,
                "--per-page",
                "1",
                "--output",
                "json",
            ];
            let listed = run(glab, root, &list, None, REMOTE).await?;
            let listed: Vec<Listed> = serde_json::from_str(&listed).map_err(|e| {
                GitOutcome::Failed { said: format!("glab listed no merge requests: {e}") }
            })?;
            let Some(merged) = listed.first() else { return Ok(None) };
            let iid = merged.iid.to_string();
            run(glab, root, &["mr", "view", &iid, "--output", "json"], None, REMOTE).await?
        }
        Err(other) => return Err(other),
    };
    let pipeline = request(&view)?.head_pipeline.map(|p| p.id);
    let jobs = match pipeline {
        Some(id) => {
            let route = format!("projects/:id/pipelines/{id}/jobs?per_page={JOBS_PAGE}");
            let read = run(glab, root, &["api", &route], None, REMOTE).await;
            read.inspect_err(|e| tracing::debug!(?e, "a pipeline's jobs not read")).ok()
        }
        None => None,
    };
    read(&view, jobs.as_deref()).map(Some)
}

/// What `glab mr create` is given: the title and description, or both from the commits with
/// an empty title; the target branch; a draft. It never waits on a prompt.
pub(super) fn create_args<'a>(
    (title, body): (&'a str, &'a str),
    base: Option<&'a str>,
    draft: bool,
) -> Vec<&'a str> {
    let mut args = vec!["mr", "create", "--yes"];
    if title.trim().is_empty() {
        args.push("--fill");
    } else {
        args.extend(["--title", title.trim(), "--description", body]);
    }
    if let Some(base) = base {
        args.extend(["--target-branch", base]);
    }
    if draft {
        args.push("--draft");
    }
    args
}

/// What `glab mr merge` is given for `method` (`merge`, `squash` or `rebase`): now, or once its
/// pipeline passes when `auto`, only at `head` when given, removing the branch when asked.
pub(super) fn merge_args(
    method: &str,
    head: Option<&str>,
    delete_branch: bool,
    auto: bool,
) -> Vec<String> {
    let auto = if auto { "--auto-merge=true" } else { "--auto-merge=false" };
    let mut args: Vec<String> = ["mr", "merge", "--yes", auto].map(str::to_owned).to_vec();
    match method {
        "squash" => args.push("--squash".to_owned()),
        "rebase" => args.push("--rebase".to_owned()),
        _ => {}
    }
    if let Some(head) = head {
        args.extend(["--sha".to_owned(), head.to_owned()]);
    }
    if delete_branch {
        args.push("--remove-source-branch".to_owned());
    }
    args
}

fn request(view: &str) -> Result<Request, GitOutcome> {
    serde_json::from_str(view)
        .map_err(|e| GitOutcome::Failed { said: format!("glab answered no merge request: {e}") })
}

/// A merge request (`view`, `glab mr view --output json`) and its pipeline's `jobs`, when they
/// were read, in a pull request's words.
///
/// # Errors
/// `view` is not the JSON asked for.
pub(super) fn read(view: &str, jobs: Option<&str>) -> Result<PullStatus, GitOutcome> {
    let request = request(view)?;
    let jobs: Option<Vec<Job>> = jobs.and_then(|j| serde_json::from_str(j).ok());
    let all: Vec<PullCheck> = match (jobs, &request.head_pipeline) {
        (Some(jobs), _) => jobs
            .into_iter()
            .map(|job| PullCheck {
                state: job_state(&job.status, job.allow_failure).to_owned(),
                name: job.name,
                workflow: Some(job.stage).filter(|s| !s.is_empty()),
                link: job.web_url,
            })
            .collect(),
        (None, Some(pipeline)) => vec![PullCheck {
            name: "pipeline".to_owned(),
            workflow: None,
            state: job_state(&pipeline.status, false).to_owned(),
            link: pipeline.web_url.clone(),
        }],
        (None, None) => Vec::new(),
    };
    let more = u32::try_from(all.len().saturating_sub(CHECKS_MAX)).unwrap_or(u32::MAX);
    let detailed = request.detailed_merge_status.as_str();
    let mergeable = if request.has_conflicts || request.merge_status == "cannot_be_merged" {
        "CONFLICTING"
    } else if request.merge_status == "can_be_merged" {
        "MERGEABLE"
    } else {
        "UNKNOWN"
    };
    let review = match detailed {
        "not_approved" => "REVIEW_REQUIRED",
        "requested_changes" => "CHANGES_REQUESTED",
        _ => "",
    };
    Ok(PullStatus {
        forge: Forge::GitLab,
        number: request.iid,
        url: request.web_url,
        title: request.title,
        state: match request.state.as_str() {
            "merged" => "MERGED",
            "closed" => "CLOSED",
            _ => "OPEN",
        }
        .to_owned(),
        draft: request.draft,
        head: request.source_branch,
        head_commit: request.sha,
        base: request.target_branch,
        review: review.to_owned(),
        mergeable: mergeable.to_owned(),
        merge_state: merge_state(detailed).to_owned(),
        checks: all.into_iter().take(CHECKS_MAX).collect(),
        more_checks: more,
        methods: Vec::new(),
        method: String::new(),
    })
}

/// A job's or a pipeline's status in a check's words: a failure allowed to fail is neutral, a
/// manual job never started is skipped, and anything under way or new is pending.
fn job_state(status: &str, allow_failure: bool) -> &'static str {
    match status {
        "success" => "SUCCESS",
        "failed" if allow_failure => "NEUTRAL",
        "failed" => "FAILURE",
        "canceled" | "canceling" => "CANCELLED",
        "skipped" | "manual" => "SKIPPED",
        "running" => "IN_PROGRESS",
        _ => "PENDING",
    }
}

/// GitLab's `detailed_merge_status` as GitHub's `mergeStateStatus` sums it up.
fn merge_state(detailed: &str) -> &'static str {
    match detailed {
        "mergeable" => "CLEAN",
        "conflict" => "DIRTY",
        "need_rebase" => "BEHIND",
        "draft_status" => "DRAFT",
        "checking" | "unchecked" | "preparing" | "approvals_syncing" | "" => "UNKNOWN",
        _ => "BLOCKED",
    }
}

/// One discussion of a merge request, as `GET projects/:id/merge_requests/:iid/discussions`
/// lists it.
#[derive(Debug, Deserialize)]
struct Discussion {
    #[serde(default)]
    notes: Vec<Note>,
}

#[derive(Debug, Deserialize)]
struct Note {
    #[serde(default)]
    body: String,
    author: Option<NoteAuthor>,
    /// Written by GitLab itself: "added 2 commits", "approved this merge request".
    #[serde(default)]
    system: bool,
    /// It can be resolved: a thread, rather than a comment that stands on its own.
    #[serde(default)]
    resolvable: bool,
    #[serde(default)]
    resolved: bool,
    position: Option<Position>,
}

#[derive(Debug, Deserialize)]
struct NoteAuthor {
    username: String,
}

/// Where a diff note is: the file and line of the change's new side, else its old side's.
#[derive(Debug, Deserialize)]
struct Position {
    new_path: Option<String>,
    new_line: Option<u32>,
    old_path: Option<String>,
    old_line: Option<u32>,
}

/// The most discussions of a merge request read: past it the rest are not asked for.
const DISCUSSIONS_PAGE: &str = "100";

/// The review still open on merge request `number`, read with `glab` under the project it
/// resolves from the checkout at `root`: each thread not resolved.
///
/// # Errors
/// glab says something other than its answer.
pub(super) async fn comments(
    glab: &Path,
    root: &Path,
    number: u32,
) -> Result<PullComments, GitOutcome> {
    let route =
        format!("projects/:id/merge_requests/{number}/discussions?per_page={DISCUSSIONS_PAGE}");
    let out = run(glab, root, &["api", &route], None, REMOTE).await?;
    read_comments(&out, number)
}

/// glab's discussions of merge request `number`, as the review still open: a thread that can be
/// resolved and is not, in its notes' order, GitLab's own notes left out.
///
/// # Errors
/// It is not the answer asked for.
pub(super) fn read_comments(out: &str, number: u32) -> Result<PullComments, GitOutcome> {
    let discussions: Vec<Discussion> = serde_json::from_str(out)
        .map_err(|e| GitOutcome::Failed { said: format!("glab listed no discussions: {e}") })?;
    let threads = discussions
        .into_iter()
        .filter_map(|d| {
            let first = d.notes.first()?;
            if first.system || !first.resolvable || first.resolved {
                return None;
            }
            let at = first.position.as_ref();
            let path = at.and_then(|p| p.new_path.clone().or_else(|| p.old_path.clone()));
            let line = at.and_then(|p| p.new_line.or(p.old_line));
            let notes = d
                .notes
                .iter()
                .filter(|n| !n.system)
                .map(|n| {
                    let author = n.author.as_ref().map_or("someone", |a| a.username.as_str());
                    super::comments::note(author, &n.body)
                })
                .collect();
            Some(PullThread { path, line, outdated: false, url: None, notes })
        })
        .collect();
    Ok(super::comments::bounded(number, threads, 0))
}
