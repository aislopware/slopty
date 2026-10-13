//! The chrome's git vocabulary: Octicons' git glyphs, drawn as every chrome glyph is
//! ([`super::Symbol`]), the node-and-line glyphs developers read as git. A draft and a closed
//! pull request each have their own glyph and their own ink ([`GitGlyph::state_ink`])
//! (`docs/decisions/ui.md`, "The chrome's icons are Octicons at 14").

use slopty_theme::{Rgb, Theme};

use super::Symbol;

/// One of the git glyphs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GitGlyph {
    /// A branch: `git-branch`.
    Branch,
    /// An open pull request: `git-pull-request`.
    PullRequest,
    /// A draft pull request: `git-pull-request-draft`, grey.
    PullRequestDraft,
    /// A pull request closed without a merge: `git-pull-request-closed`, red.
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
            Self::PullRequestDraft => Symbol::PullDraft,
            Self::PullRequestClosed => Symbol::PullClosed,
            Self::Merge => Symbol::ArrowTriangleMerge,
            Self::Commit => Symbol::GitCommit,
            Self::Repo => Symbol::GitRepo,
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

    /// Each git glyph is Octicons'; the three pull requests have a glyph and an ink each.
    #[test]
    fn each_git_glyph_is_octicons() {
        let names: Vec<&str> = GitGlyph::ALL.iter().map(|g| g.symbol().name()).collect();
        assert_eq!(
            names,
            [
                "octicons/git-branch-16",
                "octicons/git-pull-request-16",
                "octicons/git-pull-request-draft-16",
                "octicons/git-pull-request-closed-16",
                "octicons/git-merge-16",
                "octicons/git-commit-16",
                "octicons/repo-16"
            ]
        );
        let theme = Theme::default();
        let inks: Vec<_> = [GitGlyph::PullRequest, GitGlyph::PullRequestDraft]
            .iter()
            .chain(&[GitGlyph::PullRequestClosed])
            .map(|g| g.state_ink(&theme))
            .collect();
        assert!(inks.iter().all(Option::is_some), "{inks:?}");
        assert!(inks[0] != inks[1] && inks[1] != inks[2] && inks[0] != inks[2], "{inks:?}");
    }
}
