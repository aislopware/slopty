//! The person's review of a pull request, posted through their own `gh` or `glab` as one
//! review ([`slopty_proto::git::GitOp::PullReview`]).
//!
//! - **GitHub.** One `POST repos/{owner}/{repo}/pulls/:number/reviews` with `gh api`: their word
//!   over the whole, its verdict, and every note on its line and side, anchored at the head the
//!   person reviewed.
//! - **GitLab.** Each note is a draft note on its line, then all are published together with the
//!   words over the whole (`draft_notes/bulk_publish`), so the merge request gets one review and
//!   one email, as GitLab's own "Submit review" does. A request for changes or a comment sets the
//!   person's reviewer state; an approval approves at the head reviewed. A note's position names
//!   both of its lines where the diff keeps the line, as GitLab asks, read from the merge request's
//!   own diffs.
//! - **Nothing else moves.** The head is checked before anything is posted, and a review reviewed
//!   at an older head is refused. On GitLab a review is refused while the person has draft notes of
//!   their own pending there, which publishing would send too; and the drafts this made are
//!   discarded when a later step fails.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};
use slopty_proto::git::{
    Forge, GitDone, GitOutcome, LineSide, MESSAGE_MAX, NOTE_MAX, REVIEW_NOTES_MAX, ReviewNote,
    ReviewVerdict,
};

use super::{REMOTE, run};

/// What the person posts ([`slopty_proto::git::GitOp::PullReview`], unpacked).
#[derive(Clone, Debug)]
pub struct Review {
    /// The pull request's number.
    pub number: u32,
    /// What it says of the whole.
    pub verdict: ReviewVerdict,
    /// Their words over the whole.
    pub body: String,
    /// Their notes on lines.
    pub notes: Vec<ReviewNote>,
    /// The head commit they reviewed.
    pub head: Option<String>,
}

/// Post `review` through `program`, the command line of `forge`, in the checkout at `root`.
///
/// # Errors
/// The review is out of bounds or says nothing, the pull request moved past the head reviewed,
/// or the forge refused, in its words.
pub(super) async fn post(
    forge: Forge,
    program: &Path,
    root: &Path,
    review: &Review,
) -> Result<GitDone, GitOutcome> {
    check(review, forge)?;
    match forge {
        Forge::GitHub => github(program, root, review).await,
        Forge::GitLab => gitlab(program, root, review).await,
    }
}

/// A refusal in `why`'s words.
const fn refused(why: String) -> GitOutcome {
    GitOutcome::Refused { why }
}

/// Whether `review` is one the forge can take: within its bounds, each note on a line, and
/// something said.
fn check(review: &Review, forge: Forge) -> Result<(), GitOutcome> {
    if review.notes.len() > REVIEW_NOTES_MAX {
        return Err(refused(format!(
            "a review posts at most {REVIEW_NOTES_MAX} notes on lines; this has {}",
            review.notes.len()
        )));
    }
    if review.body.len() > MESSAGE_MAX {
        return Err(refused(format!("a review's words are at most {MESSAGE_MAX} bytes")));
    }
    for note in &review.notes {
        if note.body.trim().is_empty() || note.body.len() > NOTE_MAX {
            return Err(refused(format!(
                "the note on {}:{} has {} bytes; a note has 1 to {NOTE_MAX}",
                note.path,
                note.line,
                note.body.trim().len()
            )));
        }
        if note.line == 0 || note.path.is_empty() {
            return Err(refused(format!("{:?} line {} is no line", note.path, note.line)));
        }
    }
    let silent = review.body.trim().is_empty() && review.notes.is_empty();
    if silent && review.verdict != ReviewVerdict::Approve {
        return Err(refused(format!(
            "the review says nothing: write words over the {} or a note on a line",
            forge.noun()
        )));
    }
    Ok(())
}

/// Refused when the person reviewed `head` and the pull request now ends at `now`.
fn moved(head: Option<&str>, now: &str, number: u32) -> Result<(), GitOutcome> {
    let Some(head) = head.map(str::trim).filter(|h| !h.is_empty()) else { return Ok(()) };
    let same = !now.is_empty() && (now.starts_with(head) || head.starts_with(now));
    if same {
        return Ok(());
    }
    let short = |c: &str| c.chars().take(8).collect::<String>();
    Err(refused(format!(
        "#{number} has moved on to {} since {} was reviewed; read it again before posting",
        short(now),
        short(head)
    )))
}

/// What `gh pr view --json headRefOid,url` says.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    head_ref_oid: String,
    url: String,
}

/// What GitHub answers a review posted with, as far as it is read.
#[derive(Debug, Deserialize)]
struct Posted {
    html_url: Option<String>,
}

/// GitHub's word for `verdict`.
const fn event(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Comment => "COMMENT",
        ReviewVerdict::Approve => "APPROVE",
        ReviewVerdict::RequestChanges => "REQUEST_CHANGES",
    }
}

/// GitHub's word for `side`.
const fn github_side(side: LineSide) -> &'static str {
    match side {
        LineSide::Old => "LEFT",
        LineSide::New => "RIGHT",
    }
}

/// The review GitHub is asked to make, at `head`.
fn github_body(review: &Review, head: &str) -> Value {
    let comments: Vec<Value> = review
        .notes
        .iter()
        .map(|n| {
            json!({
                "path": n.path,
                "line": n.line,
                "side": github_side(n.side),
                "body": n.body,
            })
        })
        .collect();
    let mut body = json!({
        "commit_id": head,
        "event": event(review.verdict),
        "comments": comments,
    });
    if !review.body.trim().is_empty()
        && let Some(fields) = body.as_object_mut()
    {
        fields.insert("body".to_owned(), Value::String(review.body.clone()));
    }
    body
}

/// Post `review` with `gh`.
async fn github(gh: &Path, root: &Path, review: &Review) -> Result<GitDone, GitOutcome> {
    let number = review.number.to_string();
    let view = ["pr", "view", &number, "--json", "headRefOid,url"];
    let viewed = run(gh, root, &view, None, REMOTE).await?;
    let viewed: Viewed = serde_json::from_str(&viewed).map_err(|e| GitOutcome::Failed {
        said: format!("gh answered no pull request #{number}: {e}"),
    })?;
    moved(review.head.as_deref(), &viewed.head_ref_oid, review.number)?;
    let route = format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews");
    let body = github_body(review, &viewed.head_ref_oid).to_string();
    let args = ["api", "--method", "POST", &route, "--input", "-"];
    let out = run(gh, root, &args, Some(&body), REMOTE).await?;
    let posted: Option<Posted> = serde_json::from_str(&out).ok();
    Ok(GitDone::PullReviewed {
        url: posted.and_then(|p| p.html_url).or(Some(viewed.url)),
        posted: count(review),
    })
}

/// How many notes on lines went with `review`.
fn count(review: &Review) -> u32 {
    u32::try_from(review.notes.len()).unwrap_or(u32::MAX)
}

/// A merge request as `GET projects/:id/merge_requests/:iid` answers, as far as it is read.
#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    sha: String,
    #[serde(default)]
    web_url: String,
    diff_refs: Option<DiffRefs>,
}

/// The three commits a GitLab diff note is placed against.
#[derive(Debug, Deserialize)]
struct DiffRefs {
    #[serde(rename = "base_sha")]
    base: String,
    #[serde(rename = "start_sha")]
    start: String,
    #[serde(rename = "head_sha")]
    head: String,
}

/// One file of a merge request's diffs, as `GET …/diffs` lists it.
#[derive(Debug, Deserialize)]
struct FileDiff {
    old_path: String,
    new_path: String,
    #[serde(default)]
    diff: String,
}

/// A draft note, as far as its id.
#[derive(Debug, Deserialize)]
struct Draft {
    id: u64,
}

/// Post `review` with `glab`.
async fn gitlab(glab: &Path, root: &Path, review: &Review) -> Result<GitDone, GitOutcome> {
    let number = review.number;
    let base = format!("projects/:id/merge_requests/{number}");
    let read = run(glab, root, &["api", &base], None, REMOTE).await?;
    let request: Request = serde_json::from_str(&read).map_err(|e| GitOutcome::Failed {
        said: format!("glab answered no merge request !{number}: {e}"),
    })?;
    moved(review.head.as_deref(), &request.sha, number)?;
    let drafts = format!("{base}/draft_notes");
    let pending = run(glab, root, &["api", &drafts], None, REMOTE).await?;
    let pending: Vec<Draft> = serde_json::from_str(&pending)
        .map_err(|e| GitOutcome::Failed { said: format!("glab listed no draft notes: {e}") })?;
    if !pending.is_empty() {
        return Err(refused(format!(
            "{} draft notes of yours already wait on !{number}; publishing this review would \
             send them too, so publish or discard them in GitLab first",
            pending.len()
        )));
    }
    let mut made = Vec::with_capacity(review.notes.len());
    let posted = draft_and_publish(glab, root, review, (&base, &request), &mut made).await;
    if let Err(failed) = posted {
        discard(glab, root, &drafts, &made).await;
        return Err(failed);
    }
    if review.verdict == ReviewVerdict::Approve {
        let approve = format!("{base}/approve");
        let sha = format!("sha={}", request.sha);
        run(glab, root, &["api", "--method", "POST", &approve, "-f", &sha], None, REMOTE).await?;
    }
    let url = Some(request.web_url).filter(|u| !u.is_empty());
    Ok(GitDone::PullReviewed { url, posted: count(review) })
}

/// Each note of `review` made a draft note on its line, the ids into `made`, then all published
/// with the words over the whole and the reviewer state its verdict sets.
async fn draft_and_publish(
    glab: &Path,
    root: &Path,
    review: &Review,
    (base, request): (&str, &Request),
    made: &mut Vec<u64>,
) -> Result<(), GitOutcome> {
    let drafts = format!("{base}/draft_notes");
    let post = ["api", "--method", "POST", &drafts, "--input", "-"];
    if !review.notes.is_empty() {
        let refs = request.diff_refs.as_ref().ok_or_else(|| GitOutcome::Failed {
            said: format!("GitLab named no diff of !{} to place notes on", review.number),
        })?;
        let files = diffs(glab, root, base).await?;
        for note in &review.notes {
            let body = draft_body(note, refs, &files).to_string();
            let out = run(glab, root, &post, Some(&body), REMOTE).await?;
            let draft: Draft = serde_json::from_str(&out).map_err(|e| GitOutcome::Failed {
                said: format!("glab made no draft note: {e}"),
            })?;
            made.push(draft.id);
        }
    }
    let publish = format!("{base}/draft_notes/bulk_publish");
    let body = publish_body(review).to_string();
    let args = ["api", "--method", "POST", &publish, "--input", "-"];
    run(glab, root, &args, Some(&body), REMOTE).await.map(drop)
}

/// The merge request's diffs, every page.
async fn diffs(glab: &Path, root: &Path, base: &str) -> Result<Vec<FileDiff>, GitOutcome> {
    let route = format!("{base}/diffs?per_page=100");
    let args = ["api", "--paginate", "--output", "ndjson", &route];
    let out = run(glab, root, &args, None, REMOTE).await?;
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .map_err(|e| GitOutcome::Failed { said: format!("glab listed no diffs: {e}") })
        })
        .collect()
}

/// The draft note GitLab is asked for: `note` on its line, at `refs`, its paths and lines
/// read from the file's diff among `files`.
fn draft_body(note: &ReviewNote, refs: &DiffRefs, files: &[FileDiff]) -> Value {
    let file = files.iter().find(|f| match note.side {
        LineSide::New => f.new_path == note.path,
        LineSide::Old => f.old_path == note.path,
    });
    let (old_path, new_path) =
        file.map_or((note.path.as_str(), note.path.as_str()), |f| (&f.old_path, &f.new_path));
    let (old_line, new_line) = lines_of(file.map_or("", |f| f.diff.as_str()), note.side, note.line);
    let mut position = json!({
        "position_type": "text",
        "base_sha": refs.base,
        "start_sha": refs.start,
        "head_sha": refs.head,
        "old_path": old_path,
        "new_path": new_path,
    });
    if let Some(fields) = position.as_object_mut() {
        let lines = [("old_line", old_line), ("new_line", new_line)];
        for (key, line) in lines.into_iter().filter_map(|(k, l)| Some((k, l?))) {
            fields.insert(key.to_owned(), Value::from(line));
        }
    }
    json!({ "note": note.body, "position": position })
}

/// What publishing says: the words over the whole, and the reviewer state of a request for
/// changes or a comment (an approval approves on its own, after).
fn publish_body(review: &Review) -> Value {
    let mut body = serde_json::Map::new();
    if !review.body.trim().is_empty() {
        body.insert("note".to_owned(), Value::String(review.body.clone()));
    }
    let state = match review.verdict {
        ReviewVerdict::RequestChanges => Some("requested_changes"),
        ReviewVerdict::Comment => Some("reviewed"),
        ReviewVerdict::Approve => None,
    };
    if let Some(state) = state {
        body.insert("reviewer_state".to_owned(), Value::String(state.to_owned()));
    }
    Value::Object(body)
}

/// Delete the draft notes `made`, best effort: what fails to go stays a draft only the person
/// sees.
async fn discard(glab: &Path, root: &Path, drafts: &str, made: &[u64]) {
    for id in made {
        let route = format!("{drafts}/{id}");
        let _gone = run(glab, root, &["api", "--method", "DELETE", &route], None, REMOTE).await;
    }
}

/// The old and new line GitLab places a note on, for `line` on `side` of the unified `diff`: one
/// of them on a line added or removed, both on a line the diff keeps, inside a hunk or between
/// them.
fn lines_of(diff: &str, side: LineSide, line: u32) -> (Option<u32>, Option<u32>) {
    // The next line number on each side, as the walk reaches it.
    let (mut old, mut new) = (1_u32, 1_u32);
    let mut in_hunk = false;
    for text in diff.lines() {
        if let Some((old_start, new_start)) = hunk_start(text) {
            let start = match side {
                LineSide::Old => old_start,
                LineSide::New => new_start,
            };
            if line < start {
                break;
            }
            (old, new, in_hunk) = (old_start, new_start, true);
            continue;
        }
        if !in_hunk {
            continue;
        }
        let here = match side {
            LineSide::Old => old,
            LineSide::New => new,
        };
        match text.as_bytes().first() {
            Some(b'+') => {
                if side == LineSide::New && here == line {
                    return (None, Some(line));
                }
                new = new.saturating_add(1);
            }
            Some(b'-') => {
                if side == LineSide::Old && here == line {
                    return (Some(line), None);
                }
                old = old.saturating_add(1);
            }
            Some(b'\\') => {}
            _ => {
                if here == line {
                    return (Some(old), Some(new));
                }
                (old, new) = (old.saturating_add(1), new.saturating_add(1));
            }
        }
    }
    // Kept, outside every hunk: shifted by what the hunks before it added and removed.
    let shifted = |from: u32, to: u32| {
        let at = i64::from(line).saturating_sub(i64::from(from)).saturating_add(i64::from(to));
        u32::try_from(at).ok().filter(|&l| l > 0)
    };
    match side {
        LineSide::Old => (Some(line), shifted(old, new)),
        LineSide::New => (shifted(new, old), Some(line)),
    }
}

/// The first old and new line of a hunk, from its `@@ -a,b +c,d @@` header.
fn hunk_start(text: &str) -> Option<(u32, u32)> {
    let ranges = text.strip_prefix("@@ -")?;
    let (old, rest) = ranges.split_once(" +")?;
    let new = rest.split_once(' ').map_or(rest, |(n, _)| n);
    let first = |r: &str| r.split_once(',').map_or(r, |(s, _)| s).parse().ok();
    Some((first(old)?, first(new)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "@@ -3,4 +3,5 @@ fn main\n a\n-b\n+c\n+d\n e\n@@ -20,2 +21,1 @@\n f\n-g\n";

    /// A note's lines on GitLab: an added line names its new line, a removed one its old, a
    /// kept one both, inside a hunk or outside, shifted by the hunks before it.
    #[test]
    fn a_note_is_placed_on_both_lines_where_the_diff_keeps_its_line() {
        let cases = [
            (LineSide::New, 5, (None, Some(5))),
            (LineSide::Old, 4, (Some(4), None)),
            (LineSide::New, 3, (Some(3), Some(3))),
            (LineSide::New, 6, (Some(5), Some(6))),
            (LineSide::Old, 1, (Some(1), Some(1))),
            (LineSide::New, 10, (Some(9), Some(10))),
            (LineSide::Old, 21, (Some(21), None)),
            (LineSide::Old, 30, (Some(30), Some(30))),
            (LineSide::New, 40, (Some(40), Some(40))),
        ];
        for (side, line, want) in cases {
            assert_eq!(lines_of(DIFF, side, line), want, "{side:?} {line}");
        }
        assert_eq!(lines_of("", LineSide::New, 7), (Some(7), Some(7)));
    }

    /// A review that says nothing, or past its bounds, is refused before the forge is asked.
    #[test]
    fn a_review_out_of_bounds_or_saying_nothing_is_refused() {
        let note = ReviewNote {
            path: "a.rs".to_owned(),
            line: 1,
            side: LineSide::New,
            body: "x".to_owned(),
        };
        let review = |verdict, body: &str, notes: Vec<ReviewNote>| Review {
            number: 7,
            verdict,
            body: body.to_owned(),
            notes,
            head: None,
        };
        let refused =
            |r: &Review| matches!(check(r, Forge::GitHub), Err(GitOutcome::Refused { .. }));
        assert!(refused(&review(ReviewVerdict::Comment, " ", vec![])));
        assert!(refused(&review(ReviewVerdict::RequestChanges, "", vec![])));
        assert!(!refused(&review(ReviewVerdict::Approve, "", vec![])));
        assert!(!refused(&review(ReviewVerdict::Comment, "", vec![note.clone()])));
        let empty = ReviewNote { body: " ".to_owned(), ..note.clone() };
        assert!(refused(&review(ReviewVerdict::Comment, "", vec![empty])));
        let nowhere = ReviewNote { line: 0, ..note.clone() };
        assert!(refused(&review(ReviewVerdict::Comment, "", vec![nowhere])));
        let many = vec![note; REVIEW_NOTES_MAX.saturating_add(1)];
        assert!(refused(&review(ReviewVerdict::Comment, "", many)));
    }

    /// A short head names the same commit as its long one; another commit is a move.
    #[test]
    fn a_review_at_an_older_head_is_refused() {
        assert!(matches!(moved(Some("0123abcd"), "0123abcdef01", 7), Ok(())));
        assert!(matches!(moved(None, "0123abcdef01", 7), Ok(())));
        assert!(matches!(moved(Some("fedc"), "0123abcd", 7), Err(GitOutcome::Refused { .. })));
    }
}
