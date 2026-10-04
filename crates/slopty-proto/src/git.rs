//! A folder's git repository, as the person works it from a thread or a review.
//!
//! Its changed files, a commit of the files they chose with their own message, a push of the
//! branch, and a pull request through their own `gh` (`docs/decisions/projects.md`, "The
//! person commits, pushes and opens a pull request from any thread").
//!
//! Asked of the worker straight (`ClientMsg::Git`, answered with `WorkerMsg::GitDone`), by
//! the app alone: an agent commits with its own git. The worker runs the person's own
//! git and gh, with their configuration, hooks and credential helpers as they are: Slopty
//! touches no credential and makes up no message. A refusal is in git's or gh's own words.

use serde::{Deserialize, Serialize};

/// The most files a status names; the rest are counted ([`GitStatus::more`]).
pub const FILES_MAX: usize = 2000;
/// The longest commit message or pull request body taken, in bytes.
pub const MESSAGE_MAX: usize = 64 * 1024;
/// The most of git's or gh's words a refusal keeps, in bytes: the end, where they say why.
pub const SAID_MAX: usize = 4000;
/// The most checks a pull request's status names; the rest are counted
/// ([`PullStatus::more_checks`]).
pub const CHECKS_MAX: usize = 200;

/// What to do in the repository.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum GitOp {
    /// Its branch, upstream and changed files ([`GitDone::Status`]).
    Status,
    /// Commit `paths` as they are in the working tree, and nothing else staged, with the
    /// person's `message`. A path new to git is added; a path gone is removed.
    Commit {
        /// Repository-relative paths, as [`GitFile::path`] names them; a renamed file names
        /// both its paths.
        paths: Vec<String>,
        /// The person's message, as they wrote it.
        message: String,
    },
    /// Push the branch checked out to its upstream, or to the repository's one remote (or
    /// `origin`), setting it as the upstream, when it has none.
    Push,
    /// Open a pull request for the branch checked out, through the person's own `gh`.
    PullRequest {
        /// Its title; `gh` takes it and the body from the commits when empty (`--fill`).
        title: String,
        /// Its description.
        body: String,
        /// The branch it is to merge into; the repository's default when absent.
        base: Option<String>,
        /// Open it as a draft.
        draft: bool,
    },
    /// The pull request open for the branch checked out, as gh reads it
    /// ([`GitDone::PullStatus`]).
    PullStatus,
    /// Merge the branch's pull request through the person's own gh, on their word alone.
    Merge {
        /// How, as gh names it: `merge`, `squash` or `rebase`.
        method: String,
        /// The head commit the person looked at, in hex: gh merges only while the pull
        /// request still ends there (`--match-head-commit`).
        head: Option<String>,
        /// Delete the branch once merged, here and on the remote.
        delete_branch: bool,
    },
}

/// What a [`GitOp`] did.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum GitDone {
    /// The repository as it stands.
    Status(Box<GitStatus>),
    /// A commit made.
    Committed {
        /// Its id, in hex.
        commit: String,
        /// The branch it is on; none on a detached `HEAD`.
        branch: Option<String>,
        /// How many files it changed.
        files: u32,
    },
    /// The branch pushed.
    Pushed {
        /// The remote it went to.
        remote: String,
        /// The branch.
        branch: String,
        /// It had no upstream, and this push set it.
        upstream_set: bool,
        /// The branch's pull request as it stands after the push, when gh could say.
        pull: Option<Box<PullStatus>>,
    },
    /// A pull request opened.
    PullRequest {
        /// Where it is, as `gh` printed it.
        url: String,
    },
    /// The branch's pull request; none when the branch has none.
    PullStatus(Option<Box<PullStatus>>),
    /// The pull request merged, or queued to merge, as gh said.
    Merged {
        /// What gh said it did.
        said: String,
        /// The pull request as it stands after, when gh could say.
        pull: Option<Box<PullStatus>>,
    },
}

/// A pull request as its forge reports it through gh. The forge's words are kept as it spells
/// them, so a state it adds later is carried, not refused: only [`PullStatus::standing`] reads
/// them, to rank.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullStatus {
    /// Its number.
    pub number: u32,
    /// Its page.
    pub url: String,
    /// Its title.
    pub title: String,
    /// Where it stands: `OPEN`, `CLOSED`, `MERGED`.
    pub state: String,
    /// Still a draft.
    pub draft: bool,
    /// The branch it merges from.
    pub head: String,
    /// The commit that branch ends at, in hex.
    pub head_commit: String,
    /// The branch it merges into.
    pub base: String,
    /// What its reviewers decided: `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`; empty
    /// when no review is asked for.
    pub review: String,
    /// Whether git can merge it: `MERGEABLE`, `CONFLICTING`, `UNKNOWN` while the forge works it
    /// out.
    pub mergeable: String,
    /// What stands between it and a merge, as the forge sums it up: `CLEAN`, `BLOCKED`,
    /// `BEHIND`, `DIRTY`, `UNSTABLE`, `DRAFT`, `HAS_HOOKS`, `UNKNOWN`.
    pub merge_state: String,
    /// Its checks, at most [`CHECKS_MAX`].
    pub checks: Vec<PullCheck>,
    /// How many more checks there are.
    pub more_checks: u32,
}

/// One of a pull request's checks: a CI job, or a commit status another service set.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullCheck {
    /// Its name.
    pub name: String,
    /// The workflow it runs in, when it is a job of one.
    pub workflow: Option<String>,
    /// Where it stands, as the forge spells it: its conclusion once it ended (`SUCCESS`,
    /// `FAILURE`, `SKIPPED`, `CANCELLED`, `TIMED_OUT`, …), else its status (`QUEUED`,
    /// `IN_PROGRESS`, `PENDING`, …).
    pub state: String,
    /// Its page.
    pub link: Option<String>,
}

/// How a check came out, for ranking.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CheckBucket {
    /// It passed, or ended neutral.
    Passed,
    /// It was skipped.
    Skipped,
    /// It has not ended, or says something not known here.
    Running,
    /// It failed, timed out, was cancelled or needs an action.
    Failed,
}

impl PullCheck {
    /// How it came out.
    #[must_use]
    pub fn bucket(&self) -> CheckBucket {
        match self.state.to_ascii_uppercase().as_str() {
            "SUCCESS" | "NEUTRAL" => CheckBucket::Passed,
            "SKIPPED" => CheckBucket::Skipped,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
            | "STARTUP_FAILURE" | "STALE" => CheckBucket::Failed,
            _ => CheckBucket::Running,
        }
    }
}

/// Where a pull request stands for the person, most pressing first by [`Ord`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PullStanding {
    /// A check failed.
    Failing,
    /// It conflicts with its base.
    Conflicting,
    /// A reviewer asked for changes.
    ChangesRequested,
    /// Ready to merge: checks passed, nothing blocks it.
    Ready,
    /// Checks still run.
    Running,
    /// Waiting on a review, or on something else the forge says blocks it.
    Waiting,
    /// Still a draft.
    Draft,
    /// Merged.
    Merged,
    /// Closed without merging.
    Closed,
}

impl PullStatus {
    /// Where it stands for the person.
    #[must_use]
    pub fn standing(&self) -> PullStanding {
        let worst = self.checks.iter().map(PullCheck::bucket).max();
        match self.state.to_ascii_uppercase().as_str() {
            "MERGED" => return PullStanding::Merged,
            "CLOSED" => return PullStanding::Closed,
            _ => {}
        }
        if worst == Some(CheckBucket::Failed) {
            PullStanding::Failing
        } else if self.mergeable.eq_ignore_ascii_case("CONFLICTING") {
            PullStanding::Conflicting
        } else if self.review.eq_ignore_ascii_case("CHANGES_REQUESTED") {
            PullStanding::ChangesRequested
        } else if self.draft {
            PullStanding::Draft
        } else if worst == Some(CheckBucket::Running) {
            PullStanding::Running
        } else if self.merge_state.eq_ignore_ascii_case("CLEAN")
            || self.merge_state.eq_ignore_ascii_case("HAS_HOOKS")
            || self.merge_state.eq_ignore_ascii_case("UNSTABLE")
        {
            PullStanding::Ready
        } else {
            PullStanding::Waiting
        }
    }
}

/// A repository as `git status` sees it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GitStatus {
    /// The repository's root on the worker.
    pub root: String,
    /// The branch checked out; none on a detached `HEAD`.
    pub branch: Option<String>,
    /// The commit `HEAD` is at, in hex; none before the first commit.
    pub head: Option<String>,
    /// The branch's upstream, as git names it (`origin/main`).
    pub upstream: Option<String>,
    /// Commits the branch has that its upstream lacks.
    pub ahead: u32,
    /// Commits its upstream has that the branch lacks.
    pub behind: u32,
    /// The changed files, in git's order, at most [`FILES_MAX`].
    pub files: Vec<GitFile>,
    /// How many more changed files there are.
    pub more: u32,
}

/// One changed file.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GitFile {
    /// Its path from the repository's root.
    pub path: String,
    /// Where it was before a rename or copy.
    pub from: Option<String>,
    /// Its status in git's own two letters (`git status --porcelain=v2`): the index's, then
    /// the working tree's, `.` for unchanged; `??` untracked; `!!` ignored; an unmerged file
    /// as git's `UU`, `AA` and kin.
    pub xy: String,
}

/// How a [`GitOp`] went.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum GitOutcome {
    /// Done.
    Done(GitDone),
    /// Not tried: nothing there to do it with or to it, in words.
    Refused {
        /// Why.
        why: String,
    },
    /// A program it needs is not on the worker: git, or gh for a pull request.
    Unavailable {
        /// Which.
        program: String,
        /// In words.
        why: String,
    },
    /// git or gh ran and said no, in its own words: no upstream, a push rejected, a hook
    /// that failed.
    Failed {
        /// The end of what it said, at most [`SAID_MAX`] bytes.
        said: String,
    },
}

#[cfg(test)]
mod tests;
