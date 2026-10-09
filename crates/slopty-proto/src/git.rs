//! A folder's git repository, as the person works it from a thread or a review.
//!
//! Its changed files, a commit of the files they chose with their own message, a push of the
//! branch, and a pull request through their own `gh`, or a GitLab merge request through their
//! own `glab` ([`Forge`]; `docs/decisions/projects.md`, "The person commits, pushes and opens a
//! pull request from any thread").
//!
//! Asked of the worker straight (`ClientMsg::Git`, answered with `WorkerMsg::GitDone`), by
//! the app alone: an agent commits with its own git. The worker runs the person's own
//! git and gh, with their configuration, hooks and credential helpers as they are: Slopty
//! touches no credential and makes up no message. A refusal is in git's or gh's own words.

use serde::{Deserialize, Serialize};

use crate::thread::wire::{Against, Review};

/// The most files a status names; the rest are counted ([`GitStatus::more`]).
pub const FILES_MAX: usize = 2000;
/// The longest commit message or pull request body taken, in bytes.
pub const MESSAGE_MAX: usize = 64 * 1024;
/// The most of git's or gh's words a refusal keeps, in bytes: the end, where they say why.
pub const SAID_MAX: usize = 4000;
/// The most checks a pull request's status names; the rest are counted
/// ([`PullStatus::more_checks`]).
pub const CHECKS_MAX: usize = 200;
/// The most branches a listing names, the newest first; the rest are counted
/// ([`Branches::more`]).
pub const BRANCHES_MAX: usize = 500;
/// The most worktrees a listing names, the newest commit first; the rest are counted
/// ([`Worktrees::more`]).
pub const WORKTREES_MAX: usize = 200;
/// The most review threads a pull request's comments name; the rest are counted
/// ([`PullComments::more`]).
pub const COMMENTS_MAX: usize = 100;
/// The most notes of one review thread kept: the first, where the ask is, and the replies after.
pub const NOTES_MAX: usize = 20;
/// The longest note kept, in bytes: its start.
pub const NOTE_MAX: usize = 4000;

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
    /// The working tree's changes, new files and all, against `HEAD` or the branch's base
    /// ([`GitDone::Changes`]): a review of a folder that needs no thread. Nothing in the
    /// repository moves; its index is not touched.
    Changes {
        /// What the working tree is compared with.
        against: Against,
    },
    /// Free the worktree the folder is, one an agent works in under its clone's
    /// `.claude/worktrees/` ([`GitDone::WorktreeRemoved`]). Refused in words while anything in
    /// it is not committed or a terminal works in it. Its branch goes too once every commit on
    /// it has landed in the clone's default branch, or its pull request merged at its tip; a
    /// branch with work not landed stays, so nothing committed is lost.
    RemoveWorktree,
    /// The branches a new worktree of the repository could start from: its own and `origin`'s,
    /// the newest commit first ([`GitDone::Branches`]). Nothing is fetched; it lists what the
    /// clone knows.
    Branches,
    /// The review still open on pull request `number`, as the forge's own command line reads it
    /// ([`GitDone::PullComments`]): its review threads not resolved, and the last words of each
    /// reviewer whose latest review asked for changes or commented. Nothing on the forge moves.
    PullComments {
        /// Its number.
        number: u32,
    },
    /// The agents' worktrees of the clone the folder is in or of, under its
    /// `.claude/worktrees/`, each with its state ([`GitDone::Worktrees`]): what is not
    /// committed in it, whether a terminal works in it, and whether its work has landed. Nothing
    /// is fetched or moved; a pull request is asked of the forge only for a branch whose commits
    /// alone do not say it landed.
    Worktrees,
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
    /// The working tree's changes as a review, its scope
    /// [`ReviewScope::WorkingTree`](crate::thread::wire::ReviewScope::WorkingTree); why there is
    /// nothing to compare in [`Review::absent`].
    Changes(Box<Review>),
    /// The worktree removed.
    WorktreeRemoved {
        /// The branch it had checked out, if one.
        branch: Option<String>,
        /// Whether that branch went too: it stays while it holds work not landed.
        branch_removed: bool,
    },
    /// The branches a new worktree could start from.
    Branches(Box<Branches>),
    /// A pull request's review still open.
    PullComments(Box<PullComments>),
    /// The agents' worktrees of a clone.
    Worktrees(Box<Worktrees>),
}

/// The branches of a repository, as a start offers them for a new worktree's base.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Branches {
    /// The branch the clone has checked out; none on a detached `HEAD`. A new worktree starts
    /// from it when no base is asked for.
    pub current: Option<String>,
    /// `origin`'s default branch (`origin/HEAD`), as a branch name; none when the clone does not
    /// know it.
    pub default: Option<String>,
    /// Each branch by its name, once whether it is the clone's, `origin`'s or both, the newest
    /// commit first, at most [`BRANCHES_MAX`].
    pub list: Vec<Branch>,
    /// How many more branches there are.
    pub more: u32,
}

/// One branch a new worktree could start from.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Branch {
    /// Its name, as a base names it (`main`, `feature/x`), with no remote's prefix.
    pub name: String,
    /// The clone has it as a branch of its own.
    pub local: bool,
    /// `origin` has it, as the clone last fetched.
    pub remote: bool,
    /// When its newest commit was made, the later of the two where both have it: seconds since
    /// the Unix epoch, as git keeps them.
    pub committed: i64,
}

/// The agents' worktrees of a clone, each with its state.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Worktrees {
    /// The clone's root.
    pub clone: String,
    /// Each worktree, the newest commit first, at most [`WORKTREES_MAX`].
    pub list: Vec<AgentWorktree>,
    /// How many more there are.
    pub more: u32,
}

/// One agent's worktree and how it stands.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AgentWorktree {
    /// Where it is: the path [`GitOp::RemoveWorktree`] is asked in.
    pub path: String,
    /// The branch it has checked out; none on a detached `HEAD`.
    pub branch: Option<String>,
    /// How many files in it are not committed, new ones too.
    pub changed: u32,
    /// A terminal works in it.
    pub busy: bool,
    /// Its commits not yet in `origin`'s default branch or the one the clone has checked out,
    /// by patch, as `git cherry` reads them.
    pub ahead: u32,
    /// Its work has landed: no commit is ahead, or its pull request merged at the commit it
    /// ends at, as the forge says. A removal then takes its branch too.
    pub merged: bool,
    /// When its newest commit was made: seconds since the Unix epoch, as git keeps them.
    pub committed: i64,
}

impl AgentWorktree {
    /// Whether "Remove merged" takes it: its work landed, nothing in it is not committed, and
    /// no terminal works in it.
    #[must_use]
    pub const fn removable(&self) -> bool {
        self.merged && self.changed == 0 && !self.busy
    }
}

/// A pull request's review still open: what its agent is to address.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullComments {
    /// The pull request's number.
    pub number: u32,
    /// Each reviewer's open word, then each review thread not resolved, in the forge's order;
    /// at most [`COMMENTS_MAX`].
    pub threads: Vec<PullThread>,
    /// How many more threads there are.
    pub more: u32,
}

/// One open point of a review: a thread on a line of a file, or a reviewer's own words over
/// their review.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullThread {
    /// The file it is on, from the repository's root; none for a review's own words.
    pub path: Option<String>,
    /// The line of the file's new side it is on, when the forge says.
    pub line: Option<u32>,
    /// The code it was on has changed since.
    pub outdated: bool,
    /// Its page.
    pub url: Option<String>,
    /// What was said, the first note first; at most [`NOTES_MAX`].
    pub notes: Vec<PullNote>,
}

/// One note of a review thread.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullNote {
    /// Who wrote it, as the forge names them.
    pub author: String,
    /// What they wrote, at most [`NOTE_MAX`] bytes.
    pub body: String,
}

/// Where a repository's pull requests live, as its `origin`'s host says: a GitLab host's are
/// merge requests, read and made with `glab`; any other's are GitHub's, with `gh`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Forge {
    /// GitHub, or a GitHub Enterprise host: `gh`, pull requests, `#42`.
    GitHub,
    /// GitLab, on gitlab.com or the person's own host: `glab`, merge requests, `!42`.
    GitLab,
}

impl Forge {
    /// The forge of a remote on `host`: GitLab when the host names it (`gitlab.com`,
    /// `gitlab.example.com`), else GitHub.
    #[must_use]
    pub fn of_host(host: &str) -> Self {
        let host = host.to_ascii_lowercase();
        if host.split(['.', '-']).any(|part| part == "gitlab") {
            Self::GitLab
        } else {
            Self::GitHub
        }
    }

    /// What its requests are called, in a sentence: "pull request", "merge request".
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::GitHub => "pull request",
            Self::GitLab => "merge request",
        }
    }

    /// [`Self::noun`] at the head of a sentence or a label.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::GitHub => "Pull request",
            Self::GitLab => "Merge request",
        }
    }

    /// What goes before a request's number: `#42`, `!42`.
    #[must_use]
    pub const fn mark(self) -> char {
        match self {
            Self::GitHub => '#',
            Self::GitLab => '!',
        }
    }

    /// Its command line, which reads and makes its requests as the person signed it in.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::GitHub => "gh",
            Self::GitLab => "glab",
        }
    }
}

/// A pull request (a GitLab merge request) as its forge reports it through its own command
/// line.
///
/// The forge's words are kept as GitHub spells them, so a state it adds later is carried,
/// not refused: only [`PullStatus::standing`] reads them, to rank. A merge request's are put in
/// those words where the worker reads it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullStatus {
    /// Where it lives.
    pub forge: Forge,
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
    /// Where its pull requests live, as its `origin` says; none without one.
    pub forge: Option<Forge>,
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
