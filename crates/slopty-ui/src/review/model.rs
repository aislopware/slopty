//! A review as the tile holds it: the files by weight, the person's line comments, and what
//! they kept or put back on its way. Nothing here draws.

use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use slopty_proto::thread::wire::{FileDiff, Intent, Pick, Review, ReviewScope};
use slopty_proto::thread::{ThreadState, TurnId};

/// Which span of the thread's work the tile shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Scope {
    /// What the last turn changed: the default for an agent.
    #[default]
    LastTurn,
    /// What changed since the person last kept what they reviewed.
    SinceReviewed,
    /// Everything since the thread's first turn.
    AllTurns,
}

impl Scope {
    /// Every scope, in the switch's order.
    pub const ALL: [Self; 3] = [Self::LastTurn, Self::SinceReviewed, Self::AllTurns];

    /// Its name on the switch.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LastTurn => "Last turn",
            Self::SinceReviewed => "Since reviewed",
            Self::AllTurns => "All turns",
        }
    }

    /// What to ask the worker for, over `state`; `None` before the thread has a turn.
    #[must_use]
    pub fn wire(self, state: &ThreadState) -> Option<ReviewScope> {
        let first = state.turns.iter().map(|t| t.id).find(|t| *t != TurnId::BEFORE);
        match self {
            Self::LastTurn => state.last_turn().map(|t| ReviewScope::Turn(t.id)),
            Self::SinceReviewed => Some(ReviewScope::Kept),
            Self::AllTurns => first.map(ReviewScope::Since),
        }
    }
}

/// Lock files, by name.
const LOCKS: [&str; 10] = [
    "cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "go.sum",
    "gemfile.lock",
    "poetry.lock",
    "uv.lock",
];

/// Whether `path` is the kind of file a reviewer reads last: tests, fixtures, snapshots, locks
/// and generated code. They are listed under the rest, quieter.
#[must_use]
pub fn quiet(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let in_dir =
        |dir: &str| lower.starts_with(&format!("{dir}/")) || lower.contains(&format!("/{dir}/"));
    LOCKS.contains(&name)
        || ["tests", "test", "__tests__", "fixtures", "testdata", "snapshots", "generated", "dist"]
            .iter()
            .any(|dir| in_dir(dir))
        || [".snap", ".min.js", ".pb.go", ".g.dart", "_pb2.py"]
            .iter()
            .any(|end| name.ends_with(end))
        || ["_test.", ".test.", ".spec.", "_spec."].iter().any(|mid| name.contains(mid))
        || name.starts_with("test_")
}

/// A file's place in the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Listed {
    /// Its place in [`Review::files`].
    pub at: usize,
    /// Lines added and removed.
    pub weight: u32,
    /// A test, fixture, lock or generated file.
    pub quiet: bool,
}

/// The files of `review`, the weightiest first, the quiet ones after the rest.
#[must_use]
pub fn order(review: &Review) -> Vec<Listed> {
    let mut listed: Vec<Listed> = review
        .files
        .iter()
        .enumerate()
        .map(|(at, f)| Listed {
            at,
            weight: f.patch.added.saturating_add(f.patch.removed),
            quiet: quiet(&f.path),
        })
        .collect();
    listed.sort_by(|a, b| {
        let path = |l: &Listed| review.files.get(l.at).map(|f| f.path.as_str());
        a.quiet.cmp(&b.quiet).then(b.weight.cmp(&a.weight)).then_with(|| path(a).cmp(&path(b)))
    });
    listed
}

/// Which side of a diff a line is on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Side {
    /// The old file's line, removed.
    Old,
    /// The new file's line, or a line in both.
    New,
}

/// A comment on a line, waiting to be sent with the rest.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    /// The file.
    pub path: String,
    /// The line's number on its side.
    pub line: u32,
    /// Its side.
    pub side: Side,
    /// A hash of the line's text: where the comment belongs if the diff moves under it.
    pub anchor: u64,
    /// What the person wrote.
    pub body: String,
}

/// The hash a comment is anchored by.
#[must_use]
pub fn anchor(text: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    text.trim().hash(&mut h);
    h.finish()
}

/// The comments as one message to the agent, a line each: `path L<n>: body`, and `path
/// L<n> (removed): body` for a line only the old file had.
#[must_use]
pub fn message(comments: &[Comment]) -> String {
    comments
        .iter()
        .map(|c| {
            let side = if c.side == Side::Old { " (removed)" } else { "" };
            format!("{} L{}{side}: {}", c.path, c.line, c.body.trim())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a review is and what the person did over it.
#[derive(Clone, Debug, Default)]
pub struct Model {
    review: Option<Arc<Review>>,
    listed: Vec<Listed>,
    comments: Vec<Comment>,
}

impl Model {
    /// Show `review`. Comments whose line is still there, by its text, stay.
    pub fn set_review(&mut self, review: Arc<Review>) {
        self.listed = order(&review);
        self.comments.retain(|c| still_there(&review, c));
        self.review = Some(review);
    }

    /// The review on show.
    #[must_use]
    pub const fn review(&self) -> Option<&Arc<Review>> {
        self.review.as_ref()
    }

    /// The files, in the list's order.
    #[must_use]
    pub fn listed(&self) -> &[Listed] {
        &self.listed
    }

    /// The file at `at` in the review.
    #[must_use]
    pub fn file(&self, at: usize) -> Option<&FileDiff> {
        self.review.as_ref()?.files.get(at)
    }

    /// The comments waiting.
    #[must_use]
    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }

    /// Add a comment.
    pub fn comment(&mut self, comment: Comment) {
        if !comment.body.trim().is_empty() {
            self.comments.push(comment);
        }
    }

    /// Take a comment back.
    pub fn uncomment(&mut self, ix: usize) {
        if ix < self.comments.len() {
            self.comments.remove(ix);
        }
    }

    /// The comments as one message, and none waiting after.
    pub fn take_message(&mut self) -> Option<String> {
        if self.comments.is_empty() {
            return None;
        }
        let text = message(&self.comments);
        self.comments.clear();
        Some(text)
    }

    /// Keep or put back the file at `at`, or some of its hunks.
    #[must_use]
    pub fn pick(&self, at: usize, hunks: Vec<u32>, keep: bool) -> Option<Intent> {
        let file = self.file(at)?;
        let pick = Pick {
            path: file.path.clone(),
            from: file.from.clone(),
            stamp: file.to.clone(),
            hunks,
        };
        Some(if keep { Intent::Keep(pick) } else { Intent::Revert(pick) })
    }

    /// Keep every file: the review read through. What changes after shows under "Since
    /// reviewed".
    #[must_use]
    pub fn keep_all(&self) -> Vec<Intent> {
        self.listed.iter().filter_map(|l| self.pick(l.at, Vec::new(), true)).collect()
    }
}

/// Whether the line `comment` is on is in `review`, by its text.
fn still_there(review: &Review, comment: &Comment) -> bool {
    review.files.iter().filter(|f| f.path == comment.path).any(|f| {
        f.patch.hunks.iter().flat_map(|h| h.lines.iter()).any(|line| {
            let (sign, text) =
                (line.get(..1).unwrap_or_default(), line.get(1..).unwrap_or_default());
            let side_ok = match comment.side {
                Side::Old => sign == "-",
                Side::New => sign != "-",
            };
            side_ok && anchor(text) == comment.anchor
        })
    })
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::Patch;
    use slopty_proto::thread::detail::Hunk;

    use super::*;

    fn file(path: &str, added: u32, removed: u32) -> FileDiff {
        FileDiff {
            path: path.to_owned(),
            from: Some(format!("{path}@old")),
            to: Some(format!("{path}@new")),
            binary: false,
            patch: Patch {
                hunks: vec![Hunk {
                    old_start: 1,
                    old_lines: 2,
                    new_start: 1,
                    new_lines: 2,
                    heading: None,
                    lines: vec![" keep".to_owned(), "-old line".to_owned(), "+new line".to_owned()],
                }],
                added,
                removed,
                clipped_lines: 0,
                full: None,
            },
        }
    }

    fn review(files: Vec<FileDiff>) -> Review {
        Review { scope: ReviewScope::Kept, from: None, to: None, files, absent: None }
    }

    #[test]
    fn files_go_by_weight_with_tests_locks_and_generated_code_below() {
        let r = review(vec![
            file("src/small.rs", 1, 0),
            file("Cargo.lock", 400, 300),
            file("crates/x/tests/e2e.rs", 90, 0),
            file("src/big.rs", 40, 12),
            file("src/__snapshots__/a.snap", 5, 5),
        ]);
        let paths: Vec<&str> = order(&r).iter().map(|l| r.files[l.at].path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src/big.rs",
                "src/small.rs",
                "Cargo.lock",
                "crates/x/tests/e2e.rs",
                "src/__snapshots__/a.snap"
            ]
        );
        assert!(!quiet("src/contest.rs"), "a word holding \"test\" is not a test");
        assert!(quiet("web/app.test.ts") && quiet("pkg/x_test.go") && quiet("test_x.py"));
    }

    #[test]
    fn comments_go_as_one_message_a_line_each() {
        let comments = [
            Comment {
                path: "src/a.rs".to_owned(),
                line: 12,
                side: Side::New,
                anchor: anchor("let x = 1;"),
                body: "Name this better ".to_owned(),
            },
            Comment {
                path: "src/b.rs".to_owned(),
                line: 3,
                side: Side::Old,
                anchor: anchor("old();"),
                body: "Why did this go?".to_owned(),
            },
        ];
        assert_eq!(
            message(&comments),
            "src/a.rs L12: Name this better\nsrc/b.rs L3 (removed): Why did this go?"
        );
    }

    #[test]
    fn a_comment_stays_while_its_line_does_and_a_send_empties_them() {
        let mut model = Model::default();
        model.set_review(Arc::new(review(vec![file("src/a.rs", 1, 1)])));
        let on = |text: &str, side| Comment {
            path: "src/a.rs".to_owned(),
            line: 1,
            side,
            anchor: anchor(text),
            body: "Look".to_owned(),
        };
        model.comment(on("new line", Side::New));
        model.comment(on("old line", Side::Old));
        model.comment(Comment { body: "  ".to_owned(), ..on("keep", Side::New) });
        assert_eq!(model.comments().len(), 2, "an empty comment is not kept");
        let mut moved = file("src/a.rs", 1, 1);
        if let Some(h) = moved.patch.hunks.first_mut() {
            h.lines = vec![" keep".to_owned(), "-old line".to_owned(), "+newer line".to_owned()];
        }
        model.set_review(Arc::new(review(vec![moved])));
        assert_eq!(model.comments().len(), 1, "the comment on a line that changed goes");
        assert!(model.take_message().is_some());
        assert!(model.comments().is_empty() && model.take_message().is_none());
    }

    #[test]
    fn a_pick_names_the_file_as_the_review_showed_it() {
        let mut model = Model::default();
        model.set_review(Arc::new(review(vec![file("src/a.rs", 1, 1), file("Cargo.lock", 2, 2)])));
        let at = model.listed().first().unwrap().at;
        assert_eq!(
            model.pick(at, vec![0], false),
            Some(Intent::Revert(Pick {
                path: "src/a.rs".to_owned(),
                from: Some("src/a.rs@old".to_owned()),
                stamp: Some("src/a.rs@new".to_owned()),
                hunks: vec![0],
            }))
        );
        assert_eq!(model.keep_all().len(), 2, "mark reviewed keeps every file");
    }
}
