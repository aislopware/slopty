//! The chrome's git vocabulary: Tabler's git glyphs, drawn as every chrome glyph is
//! ([`super::Symbol`]). Developers read the node-and-line glyphs as git, as GitHub's Octicons
//! taught them (`docs/decisions/ui.md`, "The chrome's icons are Tabler's, and a file's are
//! Material's").

use slopty_proto::thread::wire::PullStands;
use slopty_theme::{Rgb, Theme};

use super::Symbol;

/// One of the git glyphs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GitGlyph {
    /// A branch: `git-branch`.
    Branch,
    /// An open pull request: `git-pull-request`.
    PullRequest,
    /// A draft pull request: `git-pull-request-draft`.
    PullRequestDraft,
    /// A pull request closed without a merge: `git-pull-request-closed`.
    PullRequestClosed,
    /// A merge, or a pull request merged: `git-merge`.
    Merge,
    /// A commit: `git-commit`.
    Commit,
    /// A repository: `repo`.
    Repo,
}

impl GitGlyph {
    /// Every glyph.
    pub const ALL: [Self; 7] = [
        Self::Branch,
        Self::PullRequest,
        Self::PullRequestDraft,
        Self::PullRequestClosed,
        Self::Merge,
        Self::Commit,
        Self::Repo,
    ];

    /// The glyph that draws it.
    #[must_use]
    pub const fn symbol(self) -> Symbol {
        match self {
            Self::Branch => Symbol::ArrowTriangleBranch,
            Self::PullRequest => Symbol::ArrowTrianglePull,
            Self::PullRequestDraft => Symbol::GitPullRequestDraft,
            Self::PullRequestClosed => Symbol::GitPullRequestClosed,
            Self::Merge => Symbol::ArrowTriangleMerge,
            Self::Commit => Symbol::GitCommit,
            Self::Repo => Symbol::GitRepo,
        }
    }

    /// A thread's pull request's glyph by where it `stands` as its worker's table says:
    /// merged, closed, a draft, else open.
    #[must_use]
    pub const fn of_stands(stands: PullStands) -> Self {
        match stands {
            PullStands::Merged => Self::Merge,
            PullStands::Closed => Self::PullRequestClosed,
            PullStands::Draft => Self::PullRequestDraft,
            PullStands::Conflicted
            | PullStands::ChecksFailed
            | PullStands::ChangesRequested
            | PullStands::Running
            | PullStands::Waiting
            | PullStands::Ready => Self::PullRequest,
        }
    }

    /// The ink a pull request's glyph wears for its state, as GitHub's and T3 Code's do: open
    /// green, merged violet, closed red, a draft grey, each in its mark's fill step
    /// (`docs/decisions/ui.md`, "State is a glyph"). `None` for the glyphs that say no state,
    /// a branch, a commit, a repository, which take their words' tier.
    #[must_use]
    pub const fn state_ink(self, theme: &Theme) -> Option<Rgb> {
        let s = &theme.surfaces;
        match self {
            Self::PullRequest => Some(s.success_fill),
            Self::Merge => Some(s.merged_fill),
            Self::PullRequestClosed => Some(s.error_fill),
            Self::PullRequestDraft => Some(s.text_muted),
            Self::Branch | Self::Commit | Self::Repo => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each git glyph is its own Tabler git glyph, a repository Tabler's book.
    #[test]
    fn each_git_glyph_is_tablers() {
        let names: Vec<&str> = GitGlyph::ALL.iter().map(|g| g.symbol().name()).collect();
        assert_eq!(
            names,
            [
                "git-branch",
                "git-pull-request",
                "git-pull-request-draft",
                "git-pull-request-closed",
                "git-merge",
                "git-commit",
                "book-2",
            ]
        );
    }
}
