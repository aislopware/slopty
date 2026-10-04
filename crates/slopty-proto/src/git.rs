//! A folder's git repository, as the person works it from a thread or a review.
//!
//! Its changed files, a commit of the files they chose with their own message, a push of the
//! branch, and a pull request through their own `gh` (`docs/decisions/projects.md`, "The
//! person commits, pushes and opens a pull request from any thread").
//!
//! Asked of the worker straight (`ClientMsg::Git`, answered with `WorkerMsg::GitDone`) or
//! through the server ([`crate::orchestration::Verb::Git`]). The worker runs the person's own
//! git and gh, with their configuration, hooks and credential helpers as they are: Slopty
//! touches no credential and makes up no message. A refusal is in git's or gh's own words.

use serde::{Deserialize, Serialize};

/// The most files a status names; the rest are counted ([`GitStatus::more`]).
pub const FILES_MAX: usize = 2000;
/// The longest commit message or pull request body taken, in bytes.
pub const MESSAGE_MAX: usize = 64 * 1024;
/// The most of git's or gh's words a refusal keeps, in bytes: the end, where they say why.
pub const SAID_MAX: usize = 4000;

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
    },
    /// A pull request opened.
    PullRequest {
        /// Where it is, as `gh` printed it.
        url: String,
    },
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
