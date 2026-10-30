//! A thread's repository, read for review: its working tree against a commit, a range of its
//! commits, and the sides of a file either showed (`crate::repo::snapshot` is the git half).
//!
//! No snapshot is taken of a turn: an agent works in its own TUI (`docs/decisions/agents.md`,
//! "TUI-first"), and a review reads the repository as it stands.

use std::path::{Path, PathBuf};

use slopty_proto::thread::ThreadId;
use slopty_proto::thread::wire::{EXPANDED_CHARS, Expanded, Review, ReviewScope};

use super::Host;

/// Why a review has nothing to compare.
pub const NOT_IN_GIT: &str = "This thread's folder is not in a git repository";

/// The repositories of the threads a host holds. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Reviews {
    host: Host,
    git: Option<PathBuf>,
}

impl Reviews {
    /// Reviews of `host`'s threads with `git`; with no git, none has anything to compare.
    #[must_use]
    pub const fn new(host: Host, git: Option<PathBuf>) -> Self {
        Self { host, git }
    }

    /// What changed in `thread`'s repository over `scope`.
    pub async fn review(&self, thread: ThreadId, scope: ReviewScope) -> Review {
        let absent = |why: &str| Review {
            scope: scope.clone(),
            from: None,
            to: None,
            files: Vec::new(),
            absent: Some(why.to_owned()),
        };
        let Some((git, root)) = self.repo(thread) else { return absent(NOT_IN_GIT) };
        let read = match &scope {
            ReviewScope::WorkingTree(against) => {
                crate::repo::snapshot::working_tree(&git, &root, against.clone()).await
            }
            ReviewScope::Range { base, head } => {
                crate::repo::snapshot::range(&git, &root, base, head).await
            }
            ReviewScope::Commit { commit } => {
                crate::repo::snapshot::commit(&git, &root, commit).await
            }
        };
        read.unwrap_or_else(|e| absent(&e.to_string()))
    }

    /// The text of blob `id` in `thread`'s repository, the side of a file a review showed, for
    /// the unchanged lines between its hunks (`ContentRef::blob`). [`Expanded::Gone`] outside
    /// git, for an id that is no blob there, a file that is not UTF-8, or one longer than
    /// [`EXPANDED_CHARS`]: its lines would be cut, so none are given.
    pub async fn blob(&self, thread: ThreadId, id: &str) -> Expanded {
        let Some((git, root)) = self.repo(thread) else { return Expanded::Gone };
        match crate::repo::snapshot::blob(&git, &root, id).await.map(String::from_utf8) {
            Ok(Ok(text)) if text.len() <= EXPANDED_CHARS => Expanded::Text(text),
            Ok(_) => Expanded::Gone,
            Err(e) => {
                tracing::debug!(%thread, "no blob {id}: {e}");
                Expanded::Gone
            }
        }
    }

    /// The git and the repository `thread` works in; `None` outside git, or with no git here.
    fn repo(&self, thread: ThreadId) -> Option<(PathBuf, PathBuf)> {
        let git = self.git.clone()?;
        let (meta, _) = self.host.meta(thread)?;
        let root = crate::repo::root_of(&crate::file::expand_home(Path::new(&meta.cwd)))?;
        Some((git, root))
    }
}
