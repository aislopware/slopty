//! A fresh-context review of a task's work before it merges (`docs/decisions/projects.md`, "A
//! reviewer reads the work with fresh eyes before it merges").
//!
//! The reviewer is a Claude Code session of its own, started by the server on the
//! orchestrator's worker in a checkout of the task's head. It knows the task's brief and the
//! diff from where the work left the target, and nothing of the conversation that wrote it.
//! Its verdict comes back through Slopty's tools as a [`ReviewVerdict`]; the person may give
//! one too, over the reviewer's.

use serde::{Deserialize, Serialize};

use crate::orchestration::TermRef;

/// The most findings a review keeps; the reviewer is asked for the few that matter.
pub const FINDINGS_MAX: usize = 8;
/// The most bytes of one finding's words.
pub const FINDING_MAX: usize = 512;
/// The most bytes of a finding's path, and of its severity word.
pub const FINDING_PATH_MAX: usize = 256;
/// The most bytes of a review's summary.
pub const REVIEW_SUMMARY_MAX: usize = 1024;
/// The checkout file the reviewer reads the diff from: `git diff <base>..<head>`, written
/// beside the work so reading it asks no permission.
pub const REVIEW_DIFF: &str = ".slopty-review.diff";

/// One thing a reviewer found, at a place in the work when it has one.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Finding {
    /// The file, relative to the repository's root.
    pub path: Option<String>,
    /// The line in the work's version of it.
    pub line: Option<u32>,
    /// How much it matters, in the reviewer's own word (`blocker`, `should`, `nit`).
    pub severity: String,
    /// Whether it keeps the work from merging.
    pub blocking: bool,
    /// What is wrong and what to do, in a few sentences.
    pub body: String,
}

/// What a reviewer, or the person, says of a task's work.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReviewVerdict {
    /// Whether it may merge.
    pub approved: bool,
    /// The review in a few lines.
    pub summary: String,
    /// What it found, the most important first.
    pub findings: Vec<Finding>,
}

/// Who gave a review.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Reviewer {
    /// The reviewer session the server started, in this terminal.
    Agent(TermRef),
    /// The person, over or instead of the reviewer.
    Person,
}

/// A review of a task's work, at the commits it read: it counts for that head only.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReviewRun {
    /// What it said, its texts clipped to [`REVIEW_SUMMARY_MAX`], [`FINDING_MAX`] and
    /// [`FINDING_PATH_MAX`], and at most [`FINDINGS_MAX`] findings.
    pub verdict: ReviewVerdict,
    /// How many findings were past [`FINDINGS_MAX`] and left out.
    pub more: u16,
    /// The commit it reviewed, in hex.
    pub head: String,
    /// Where that work left the target branch, in hex: the diff it read starts here.
    pub base: String,
    /// Who gave it.
    pub by: Reviewer,
    /// How long it took from the reviewer's start, in milliseconds; 0 for the person's.
    pub took_ms: u64,
}

impl ReviewRun {
    /// The most it takes on the wire, from the bounds on its fields.
    pub const MAX_BYTES: usize = REVIEW_SUMMARY_MAX
        + FINDINGS_MAX * (FINDING_MAX + 2 * FINDING_PATH_MAX + 24)
        + 2 * crate::project::REF_MAX
        + 96;

    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        let findings = self
            .verdict
            .findings
            .iter()
            .map(|f| {
                f.path
                    .as_deref()
                    .map_or(0, str::len)
                    .saturating_add(f.severity.len())
                    .saturating_add(f.body.len())
                    .saturating_add(24)
            })
            .fold(0_usize, usize::saturating_add);
        self.verdict
            .summary
            .len()
            .saturating_add(findings)
            .saturating_add(self.head.len())
            .saturating_add(self.base.len())
            .saturating_add(96)
    }

    /// The findings that keep the work from merging.
    pub fn blocking(&self) -> impl Iterator<Item = &Finding> {
        self.verdict.findings.iter().filter(|f| f.blocking)
    }
}
