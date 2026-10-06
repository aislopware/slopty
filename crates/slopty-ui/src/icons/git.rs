//! The chrome's git vocabulary: GitHub's Octicons at their 16 px size (`assets/git/`, MIT,
//! credited in its `NOTICE`), which SF Symbols lacks. SF's `arrow.triangle.pull` reads as a
//! lone bent arrow and its branch as a "Y"; developers read GitHub's node-and-line glyphs as
//! git (`docs/decisions/ui.md`, "An icon takes its words' size, weight and tier, and git is
//! drawn in Octicons").
//!
//! Each is filled from its outline by Core Graphics ([`slopty_platform::outline`]) into the
//! same alpha mask an SF Symbol is, and painted in the ink of the words beside it. Its 16-unit
//! grid spans [`EM`] of the symbol's point size, so its strokes fall between SF's regular and
//! medium beside the same words.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::SharedString;
use parking_lot::RwLock;
use slopty_platform::outline::{Outline, rasterize_outline};
use slopty_proto::agent::Review;
use slopty_proto::git::PullStanding;
use slopty_theme::{Rgb, Theme};

use super::Kept;

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
    /// Every glyph, in the order [`OUTLINES`] holds them.
    pub const ALL: [Self; 7] = [
        Self::Branch,
        Self::PullRequest,
        Self::PullRequestDraft,
        Self::PullRequestClosed,
        Self::Merge,
        Self::Commit,
        Self::Repo,
    ];

    /// Its Octicon's name, which names its file and its atlas key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Branch => "git-branch",
            Self::PullRequest => "git-pull-request",
            Self::PullRequestDraft => "git-pull-request-draft",
            Self::PullRequestClosed => "git-pull-request-closed",
            Self::Merge => "git-merge",
            Self::Commit => "git-commit",
            Self::Repo => "repo",
        }
    }

    /// A pull request's glyph by where it `stands`: merged, closed, a draft, else open.
    #[must_use]
    pub const fn of_pull(stands: PullStanding) -> Self {
        match stands {
            PullStanding::Merged => Self::Merge,
            PullStanding::Closed => Self::PullRequestClosed,
            PullStanding::Draft => Self::PullRequestDraft,
            PullStanding::Failing
            | PullStanding::Conflicting
            | PullStanding::ChangesRequested
            | PullStanding::Ready
            | PullStanding::Running
            | PullStanding::Waiting => Self::PullRequest,
        }
    }

    /// An agent's pull request's glyph by its `review`: a draft's, else open.
    #[must_use]
    pub const fn of_review(review: Option<Review>) -> Self {
        match review {
            Some(Review::Draft) => Self::PullRequestDraft,
            Some(Review::Approved | Review::Pending | Review::ChangesRequested) | None => {
                Self::PullRequest
            }
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

    /// Its place in [`Self::ALL`].
    const fn index(self) -> usize {
        match self {
            Self::Branch => 0,
            Self::PullRequest => 1,
            Self::PullRequestDraft => 2,
            Self::PullRequestClosed => 3,
            Self::Merge => 4,
            Self::Commit => 5,
            Self::Repo => 6,
        }
    }
}

/// The share of the symbol's point size an Octicon's 16-unit grid spans: GitHub sets its
/// 16 px glyphs beside 14 px text, so beside 13 pt words the grid spans 14.9 pt and its ink
/// stands 13 px at 1x, the height of SF's terminal and folder beside the same words.
/// Measured at 1x (`docs/MEASUREMENTS.md`, "git glyphs at 1x"), the branch's crispness is 0.79
/// against SF's 0.74 at 13 pt; drawn at the 14 pt the study first put, it was a pixel short of
/// SF and softer.
pub(super) const EM: f32 = 16.0 / 14.0;

/// The side of the grid every Octicon here is drawn on, in its own units.
const GRID: f64 = 16.0;

/// The outlines, read once from the files `assets/git/` keeps; a file that does not read is a
/// miss, said once.
static OUTLINES: LazyLock<[Option<Outline>; 7]> = LazyLock::new(|| {
    let read = |glyph: GitGlyph, svg: &str| {
        Outline::parse(&super::marks::path_data(svg))
            .inspect_err(
                |error| tracing::warn!(%error, name = glyph.name(), "a git glyph did not read"),
            )
            .ok()
    };
    [
        read(GitGlyph::Branch, include_str!("../../assets/git/git-branch-16.svg")),
        read(GitGlyph::PullRequest, include_str!("../../assets/git/git-pull-request-16.svg")),
        read(
            GitGlyph::PullRequestDraft,
            include_str!("../../assets/git/git-pull-request-draft-16.svg"),
        ),
        read(
            GitGlyph::PullRequestClosed,
            include_str!("../../assets/git/git-pull-request-closed-16.svg"),
        ),
        read(GitGlyph::Merge, include_str!("../../assets/git/git-merge-16.svg")),
        read(GitGlyph::Commit, include_str!("../../assets/git/git-commit-16.svg")),
        read(GitGlyph::Repo, include_str!("../../assets/git/repo-16.svg")),
    ]
});

/// The masks drawn so far, by glyph and the ink's longer side in device pixels.
static KEPT: LazyLock<RwLock<HashMap<(GitGlyph, u32), Kept>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// How many device pixels the longer side of `outline`'s ink takes when its grid spans `em`
/// points on a display of `device` pixels to the point.
fn ink_pixels(outline: &Outline, em: f32, device: f32) -> u32 {
    let (wide, high) = outline.ink_size();
    let pixels = (wide.max(high) / GRID * f64::from(em) * f64::from(device)).round();
    #[expect(clippy::cast_possible_truncation, reason = "a glyph is a few dozen pixels")]
    #[expect(clippy::cast_sign_loss, reason = "a negative size is caught as no size")]
    let pixels = if pixels.is_finite() && pixels > 0.0 { pixels as u32 } else { 0 };
    pixels
}

/// `glyph` with its grid `em` points across, for a display of `device` pixels to the point,
/// and its atlas key.
pub(super) fn mask(glyph: GitGlyph, em: f32, device: f32) -> Kept {
    let outline = OUTLINES.get(glyph.index())?.as_ref()?;
    let pixels = ink_pixels(outline, em, device);
    if pixels == 0 {
        return None;
    }
    let at = (glyph, pixels);
    if let Some(kept) = KEPT.read().get(&at) {
        return kept.clone();
    }
    let kept = rasterize_outline(outline, pixels)
        .map(|m| (Arc::new(m), SharedString::from(format!("git:{}:{pixels}", glyph.name()))));
    KEPT.write().entry(at).or_insert(kept).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::{Drawn, IconSize, Symbol};

    /// Every glyph's file reads, and its ink sits inside its 16-unit grid.
    #[test]
    fn every_git_glyph_reads() {
        for glyph in GitGlyph::ALL {
            assert_eq!(GitGlyph::ALL.get(glyph.index()), Some(&glyph), "{glyph:?} in its place");
            let outline = OUTLINES.get(glyph.index()).and_then(Option::as_ref);
            let Some(outline) = outline else { panic!("{} did not read", glyph.name()) };
            let (wide, high) = outline.ink_size();
            assert!(wide.max(high) > 12.0 && wide <= GRID && high <= GRID, "{glyph:?}");
        }
    }

    /// Crisp (Σα²/Σα, 1 when every inked pixel is whole) and solid (the share of inked pixels
    /// at α ≥ 0.9) of `mask`.
    fn measure(mask: &slopty_platform::symbols::SymbolMask) -> (f64, f64) {
        let (mut a2, mut a1, mut inked, mut solid) = (0.0, 0.0, 0_u32, 0_u32);
        for a in &mask.alpha {
            let a = f64::from(*a) / 255.0;
            a2 = a.mul_add(a, a2);
            a1 += a;
            inked = inked.saturating_add(u32::from(a > 0.0));
            solid = solid.saturating_add(u32::from(a >= 0.9));
        }
        (a2 / a1, f64::from(solid) / f64::from(inked))
    }

    /// The git glyphs beside 13 pt words at 1x, against SF's own glyphs beside the same words,
    /// printed for `docs/MEASUREMENTS.md` ("git glyphs at 1x"): every Octicon is within 0.02
    /// of SF's crisper of the terminal and the folder, or crisper.
    #[test]
    fn the_git_glyphs_are_crisp_at_1x() {
        let theme = Theme::default();
        let lead = Drawn::new(&theme, GitGlyph::Branch, IconSize::Lead);
        let size = lead.size(IconSize::Lead.slot(&theme));
        let mut sf = 0.0_f64;
        for symbol in [Symbol::Terminal, Symbol::Folder] {
            let (mask, ..) = crate::icons::mask(symbol, size, 1.0).expect("the OS draws it");
            let (crisp, solid) = measure(&mask);
            eprintln!(
                "SF {} {:.1} pt: crisp {crisp:.3} solid {solid:.2}",
                symbol.name(),
                size.point
            );
            sf = sf.max(crisp);
        }
        for glyph in GitGlyph::ALL {
            let (mask, _) = mask(glyph, size.point * EM, 1.0).expect("the glyph is drawn");
            let (crisp, solid) = measure(&mask);
            eprintln!(
                "{} {}x{} px: crisp {crisp:.3} solid {solid:.2}",
                glyph.name(),
                mask.width,
                mask.height
            );
            assert!(crisp + 0.02 >= sf, "{} at 1x: {crisp:.3} against SF's {sf:.3}", glyph.name());
        }
    }

    /// The coverage a pixel counts as solid at: nine tenths.
    const SOLID: u8 = 230;

    /// Beside 13 pt words a branch's ink stands 13 px at 1x, SF's height beside the same
    /// words, with solid pixels in its strokes; at 2x it is twice that.
    #[test]
    fn a_git_glyph_is_drawn_at_its_words_size() {
        let em = 13.0 * EM;
        let one = mask(GitGlyph::Branch, em, 1.0).map(|(m, _)| m);
        let two = mask(GitGlyph::Branch, em, 2.0).map(|(m, _)| m);
        let (Some(one), Some(two)) = (one, two) else { panic!("the branch is drawn") };
        assert_eq!(one.height, 13, "at 1x");
        assert!(two.height.abs_diff(one.height * 2) <= 1, "{} at 2x", two.height);
        let solid: u32 = one.alpha.iter().map(|&a| u32::from(a >= SOLID)).sum();
        assert!(solid > 10, "solid ink at 1x: {solid}");
    }
}
