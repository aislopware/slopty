//! A review as the tile holds it: the files by weight, the person's line comments, the
//! findings of the agent's own review, and what they kept or put back on its way. Nothing here
//! draws.

use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use slopty_proto::thread::wire::{FileDiff, Intent, Pick, Review, ReviewScope};
use slopty_proto::thread::{ThreadState, TurnId};

use super::findings::Finding;

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

    /// What the body says when the span holds no change: the span by name, so an empty review
    /// reads as a finding and not as a pane that failed to load.
    #[must_use]
    pub const fn nothing(self) -> &'static str {
        match self {
            Self::LastTurn => "The last turn changed no files",
            Self::SinceReviewed => "Nothing new since you last reviewed",
            Self::AllTurns => "No turn has changed a file yet",
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

/// A comment on a line or a run of lines, waiting to be sent with the rest.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    /// The file.
    pub path: String,
    /// The first line's number on its side.
    pub line: u32,
    /// The last line's number on its side: `line` for a comment on one line.
    pub end: u32,
    /// Its side.
    pub side: Side,
    /// A hash of the first line's text: where the comment belongs if the diff moves under it.
    pub anchor: u64,
    /// The lines it is on, quoted as the agent reads them
    /// ([`crate::conversation::diff::quote`]): where they are, then a fenced diff.
    pub quote: String,
    /// What the person wrote, or the agent found.
    pub body: String,
    /// The agent whose own review raised it, by name; `None` for the person's.
    pub by: Option<String>,
}

impl Comment {
    /// Where it is, in a few words: "Line 12", "Lines 12–18".
    #[must_use]
    pub fn place(&self) -> String {
        if self.end > self.line {
            format!("Lines {}\u{2013}{}", self.line, self.end)
        } else {
            format!("Line {}", self.line)
        }
    }
}

/// The hash a comment is anchored by.
#[must_use]
pub fn anchor(text: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    text.trim().hash(&mut h);
    h.finish()
}

/// A finding of the agent's own review that has no line in the diff on show: a file or lines
/// outside it, or no place at all. It is kept beside the comments, never dropped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Note {
    /// The agent whose review raised it, by name.
    pub by: String,
    /// What it found.
    pub finding: Finding,
}

impl Note {
    /// How the agent reads it: where it is, when it says, then what was found.
    #[must_use]
    pub fn message(&self) -> String {
        let text = self.finding.text();
        match &self.finding.place {
            Some(place) => format!("In `{}`:\n{}", place.words(), text.trim()),
            None => text.trim().to_owned(),
        }
    }
}

/// The comments and notes as one message to the agent.
///
/// Each comment is the code it is on, quoted with where it is, then what was written of it, so
/// the agent need not open the file to know what was meant; each note follows.
#[must_use]
pub fn message(comments: &[Comment], notes: &[Note]) -> String {
    comments
        .iter()
        .map(|c| format!("{}{}", c.quote, c.body.trim()))
        .chain(notes.iter().map(Note::message))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What a review is and what the person did over it.
#[derive(Clone, Debug, Default)]
pub struct Model {
    review: Option<Arc<Review>>,
    listed: Vec<Listed>,
    comments: Vec<Comment>,
    notes: Vec<Note>,
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

    /// The agent's findings that have no line on show.
    #[must_use]
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// Keep a finding with no line on show.
    pub fn note(&mut self, note: Note) {
        self.notes.push(note);
    }

    /// Let a note go.
    pub fn unnote(&mut self, ix: usize) {
        if ix < self.notes.len() {
            self.notes.remove(ix);
        }
    }

    /// How many comments and notes would go.
    #[must_use]
    pub const fn waiting(&self) -> usize {
        self.comments.len().saturating_add(self.notes.len())
    }

    /// Whether anything the agent's review found is still here.
    #[must_use]
    pub fn has_findings(&self) -> bool {
        !self.notes.is_empty() || self.comments.iter().any(|c| c.by.is_some())
    }

    /// The comments and notes as one message, and none waiting after.
    pub fn take_message(&mut self) -> Option<String> {
        if self.waiting() == 0 {
            return None;
        }
        let text = message(&self.comments, &self.notes);
        self.comments.clear();
        self.notes.clear();
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
    fn comments_go_as_one_message_each_under_its_code() {
        let comments = [
            Comment {
                path: "src/a.rs".to_owned(),
                line: 12,
                end: 13,
                side: Side::New,
                anchor: anchor("let x = 1;"),
                quote:
                    "In `src/a.rs` lines 12\u{2013}13:\n```diff\n+let x = 1;\n+let y = 2;\n```\n"
                        .to_owned(),
                body: "Name these better ".to_owned(),
                by: None,
            },
            Comment {
                path: "src/b.rs".to_owned(),
                line: 3,
                end: 3,
                side: Side::Old,
                anchor: anchor("old();"),
                quote: "In `src/b.rs` line 3:\n```diff\n-old();\n```\n".to_owned(),
                body: "Why did this go?".to_owned(),
                by: None,
            },
        ];
        let note = Note {
            by: "Codex".to_owned(),
            finding: Finding {
                title: "[P2] Stale doc".to_owned(),
                body: "It says v1.".to_owned(),
                place: Some(super::super::findings::Place {
                    path: "README.md".to_owned(),
                    lines: Some((4, 4)),
                }),
            },
        };
        assert_eq!(
            message(&comments, std::slice::from_ref(&note)),
            "In `src/a.rs` lines 12\u{2013}13:\n```diff\n+let x = 1;\n+let y = 2;\n```\nName these \
             better\n\nIn `src/b.rs` line 3:\n```diff\n-old();\n```\nWhy did this go?\n\nIn \
             `README.md:4`:\n[P2] Stale doc\nIt says v1."
        );
        assert_eq!(comments[0].place(), "Lines 12\u{2013}13");
        assert_eq!(comments[1].place(), "Line 3");
    }

    #[test]
    fn a_comment_stays_while_its_line_does_and_a_send_empties_them() {
        let mut model = Model::default();
        model.set_review(Arc::new(review(vec![file("src/a.rs", 1, 1)])));
        let on = |text: &str, side| Comment {
            path: "src/a.rs".to_owned(),
            line: 1,
            end: 1,
            side,
            anchor: anchor(text),
            quote: String::new(),
            body: "Look".to_owned(),
            by: None,
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
