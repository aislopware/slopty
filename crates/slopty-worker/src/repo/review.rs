//! The checkout a task's fresh-context reviewer reads (`docs/decisions/projects.md`, "A
//! reviewer reads the work with fresh eyes before it merges").
//!
//! It is a checkout of the task's head of its own under the verify places, so the project's
//! verifier and merge queue go on beside a review that takes minutes. The diff from where the
//! work left the target is written beside the work ([`REVIEW_DIFF`]), so the reviewer reads it
//! as a file in its folder, which asks the person nothing, and runs no git of its own.

use std::path::Path;

use slopty_proto::project::REVIEW_DIFF;

use super::bundle;
use super::verify::{self, Checkout, Failed};

/// `head` checked out at `place` from the clone at `repo`, with its diff from its fork point
/// off the branch `target` beside it.
///
/// # Errors
/// As [`verify::checkout`], and a diff that could not be made or written.
pub async fn checkout(
    git: &Path,
    repo: &Path,
    place: &Path,
    head: &str,
    target: &str,
) -> Result<Checkout, Failed> {
    let made = verify::checkout(git, repo, place, head, target).await?;
    let range = format!("{}..{}", made.base, made.head);
    let diff = bundle::run(
        git,
        place,
        &["diff", "--no-color", "--no-ext-diff", "--stat", "--patch", "--end-of-options", &range],
    )
    .await?;
    let text = format!("git diff {range}\n\n{diff}");
    let file = place.join(REVIEW_DIFF);
    tokio::fs::write(&file, text)
        .await
        .map_err(|e| Failed::Other(format!("{}: {e}", file.display())))?;
    Ok(made)
}

#[cfg(test)]
mod tests;
