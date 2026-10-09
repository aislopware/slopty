//! A review as the tile holds it: the files by weight, the person's line comments, the
//! findings of the agent's own review, and what they kept or put back on its way. Nothing here
//! draws.

use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use slopty_proto::thread::wire::{self, Against, FileDiff, Intent, Pick, Review, ReviewScope};
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
    /// The working tree against `HEAD`: what is not committed. A folder's default.
    Uncommitted,
    /// The working tree against where the branch left its base: all of the branch's work.
    WholeBranch,
}

impl Scope {
    /// Every scope.
    pub const ALL: [Self; 5] =
        [Self::LastTurn, Self::SinceReviewed, Self::AllTurns, Self::Uncommitted, Self::WholeBranch];
    /// A folder's scopes, in the switch's order.
    pub const FOLDER: [Self; 2] = [Self::Uncommitted, Self::WholeBranch];
    /// A thread's scopes, in the switch's order.
    pub const THREAD: [Self; 3] = [Self::LastTurn, Self::SinceReviewed, Self::AllTurns];

    /// Its name on the switch.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LastTurn => "Last turn",
            Self::SinceReviewed => "Since reviewed",
            Self::AllTurns => "All turns",
            Self::Uncommitted => "Uncommitted",
            Self::WholeBranch => "Whole branch",
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
            Self::Uncommitted => "Nothing here is uncommitted",
            Self::WholeBranch => "This branch has changed nothing since its base",
        }
    }

    /// What to ask the worker for, over `state`, the whole branch since it left `branch` where
    /// one is named (its pull request's base); `None` before the thread has a turn.
    #[must_use]
    pub fn wire(self, state: &ThreadState, branch: Option<&str>) -> Option<ReviewScope> {
        let first = state.turns.iter().map(|t| t.id).find(|t| *t != TurnId::BEFORE);
        match self {
            Self::LastTurn => state.last_turn().map(|t| ReviewScope::Turn(t.id)),
            Self::SinceReviewed => Some(ReviewScope::Kept),
            Self::AllTurns => first.map(ReviewScope::Since),
            Self::Uncommitted | Self::WholeBranch => self.wire_alone(branch),
        }
    }

    /// What to ask the worker for with no thread: a working tree's span, the whole branch
    /// since it left `branch` where one is named, else its base; `None` for a span of a
    /// thread's turns.
    #[must_use]
    pub fn wire_alone(self, branch: Option<&str>) -> Option<ReviewScope> {
        match self {
            Self::Uncommitted => Some(ReviewScope::WorkingTree(Against::Head)),
            Self::WholeBranch => Some(ReviewScope::WorkingTree(
                branch.map_or(Against::Base, |b| Against::Branch(b.to_owned())),
            )),
            Self::LastTurn | Self::SinceReviewed | Self::AllTurns => None,
        }
    }
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

/// The files of `review` in their reading order ([`wire::reading_order`]): the weightiest
/// first, the quiet ones after the rest. The worker spends the review's lines in this order.
#[must_use]
pub fn order(review: &Review) -> Vec<Listed> {
    wire::reading_order(&review.files)
        .into_iter()
        .filter_map(|at| {
            let f = review.files.get(at)?;
            Some(Listed { at, weight: f.weight(), quiet: wire::quiet(&f.path) })
        })
        .collect()
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

/// The comments and notes one message was made of, for letting them go once it is taken.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Batch {
    comments: Vec<Comment>,
    notes: Vec<Note>,
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

    /// The comments and notes as one message, with what it holds: they stay until the person's
    /// send of it is taken ([`Self::forget`]), so a send turned down loses nothing.
    #[must_use]
    pub fn message(&self) -> Option<(String, Batch)> {
        if self.waiting() == 0 {
            return None;
        }
        let text = message(&self.comments, &self.notes);
        Some((text, Batch { comments: self.comments.clone(), notes: self.notes.clone() }))
    }

    /// Let go of what `batch` held, once its message was taken: comments and notes added
    /// meanwhile stay.
    pub fn forget(&mut self, batch: &Batch) {
        for gone in &batch.comments {
            if let Some(at) = self.comments.iter().position(|c| c == gone) {
                self.comments.remove(at);
            }
        }
        for gone in &batch.notes {
            if let Some(at) = self.notes.iter().position(|n| n == gone) {
                self.notes.remove(at);
            }
        }
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
            kind: wire::FileKind::Text,
            old_path: None,
            modes: None,
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
    fn a_comment_stays_while_its_line_does_and_until_its_send_is_taken() {
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
        let (_, batch) = model.message().expect("one comment to send");
        assert_eq!(model.comments().len(), 1, "it waits until the send is taken");
        model.comment(Comment { body: "Later".to_owned(), ..on("keep", Side::New) });
        model.forget(&batch);
        assert_eq!(model.comments().len(), 1, "only what was sent goes");
        assert_eq!(model.comments()[0].body, "Later");
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
