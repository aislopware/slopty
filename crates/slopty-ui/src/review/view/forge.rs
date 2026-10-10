//! The branch's pull request in the review: its open review threads under the lines they are
//! on, and the person's own comments posted to it as their review.
//!
//! - **Threads.** A thread the forge has on a line of the new side hangs under that line, as a
//!   comment waiting does; one whose line is not in the diff on show, or whose code has changed
//!   since, hangs at the end of its file, saying which line it was on. A reviewer's words over
//!   their whole review are the commit sheet's to show ("Address the review").
//! - **Posting.** The person's own comments (not the agent's findings) go to the pull request as
//!   one review through their own `gh` or `glab` (`GitOp::PullReview`), as plain comments, an
//!   approval, or a request for changes, anchored at the head commit this client last read. They
//!   stay until the forge took them; then they go, and what was posted is said above the diff. A
//!   post turned down keeps them and says why.

use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_proto::git::{
    Forge, GitOp, LineSide, PullComments, PullThread, REVIEW_NOTES_MAX, ReviewNote, ReviewVerdict,
};
use slopty_theme::Typography;

use super::{Came, ReviewView, Row, Side, on};
use crate::colors::hsla;
use crate::conversation::diff::{Block, Line};
use crate::conversation::thread::git::Said;
use crate::icons::Symbol;
use crate::kit;
use crate::review::model::Comment;

/// The pull request the person's comments would be posted to: its number, its forge, and the
/// head commit last read, where the notes are anchored.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct PostTo {
    number: u32,
    forge: Forge,
    head: Option<String>,
}

impl PostTo {
    /// "#42", "!42".
    fn name(&self) -> String {
        format!("{}{}", self.forge.mark(), self.number)
    }
}

/// A post on its way: the op's number and the comments it took.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Posting {
    request: slopty_proto::RequestId,
    comments: Vec<Comment>,
    to: String,
}

/// The foot's post, by its key in its row.
pub(super) const FOOT_POST: &str = "post";

/// What each verdict's row in the post's menu says, and its slug.
pub(in crate::review) const VERDICTS: [(ReviewVerdict, &str, &str); 3] = [
    (ReviewVerdict::Comment, "comment", "Post as comments"),
    (ReviewVerdict::Approve, "approve", "Approve"),
    (ReviewVerdict::RequestChanges, "changes", "Request changes"),
];

/// What the foot's post says: where it goes, and how many.
#[must_use]
pub(in crate::review) fn post_words(to: &str, n: usize) -> String {
    format!("Post {} to {to}\u{2026}", kit::count(n as u64, "comment", "comments"))
}

/// What is said above the diff once the forge took the review.
#[must_use]
pub(in crate::review) fn posted_words(to: &str, posted: u32) -> String {
    format!("Posted {} to {to}", kit::count(u64::from(posted), "comment", "comments"))
}

/// A person's comment as a note of their review: on its last line, as the forge anchors a
/// note on several lines.
fn review_note(comment: &Comment) -> ReviewNote {
    ReviewNote {
        path: comment.path.clone(),
        line: comment.end,
        side: match comment.side {
            Side::Old => LineSide::Old,
            Side::New => LineSide::New,
        },
        body: comment.body.trim().to_owned(),
    }
}

/// Whether `thread` is on one of `lines` of the file at `path`.
fn on_line(thread: &PullThread, path: &str, lines: &[&Line]) -> bool {
    !thread.outdated
        && thread.path.as_deref() == Some(path)
        && thread.line.is_some_and(|n| lines.iter().any(|l| on(l, Side::New, n)))
}

impl ReviewView {
    /// The open pull request's review threads, as its repository last said; `None` without
    /// one.
    fn forge_said(&self, cx: &App) -> Option<Arc<PullComments>> {
        let repo = self.repo(cx)?;
        self.hub.read(cx).git().repo(&repo)?.comments.clone()
    }

    /// Take the pull request's threads as they last came: whether they changed, so the rows
    /// are laid out again.
    pub(super) fn take_forge(&mut self, cx: &App) -> bool {
        let came = self.forge_said(cx);
        let same = match (&came, &self.forge) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        self.forge = came;
        !same
    }

    /// The forge's threads under a line of `path` that is one of `lines`.
    pub(super) fn forge_under(&self, rows: &mut Vec<Row>, path: &str, lines: &[&Line]) {
        let Some(forge) = &self.forge else { return };
        for (ix, thread) in forge.threads.iter().enumerate() {
            if on_line(thread, path, lines) {
                rows.push(Row::Forge(ix, true));
            }
        }
    }

    /// The forge's threads on the file at `path` that hang under none of its lines on show:
    /// at the file's end.
    pub(super) fn forge_left(&self, rows: &mut Vec<Row>, path: &str, blocks: &[Block]) {
        let Some(forge) = &self.forge else { return };
        let lines: Vec<&Line> = blocks.iter().flat_map(|b| b.lines.iter()).collect();
        for (ix, thread) in forge.threads.iter().enumerate() {
            if thread.path.as_deref() == Some(path) && !on_line(thread, path, &lines) {
                rows.push(Row::Forge(ix, false));
            }
        }
    }

    /// One thread of the forge: who said what, the first note first, where it was when it is
    /// not under its line, and the way to its page.
    pub(super) fn forge_row(&self, ix: usize, under: bool) -> AnyElement {
        let Some(thread) = self.forge.as_ref().and_then(|f| f.threads.get(ix)) else {
            return div().into_any_element();
        };
        let theme = &self.theme;
        let s = theme.surfaces;
        let place = match (thread.line, thread.outdated) {
            (Some(line), true) => Some(format!("Line {line}, changed since")),
            (Some(line), false) if !under => Some(format!("Line {line}")),
            (None, true) => Some("On code changed since".to_owned()),
            _ => None,
        };
        let notes = thread.notes.iter().enumerate().map(|(n, note)| {
            div()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xxs))
                .when(n > 0, |el| el.pt(px(theme.spacing.xs)))
                .child(
                    div()
                        .text_color(hsla(s.text))
                        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                        .child(SharedString::from(note.author.clone())),
                )
                .child(
                    div()
                        .text_color(hsla(s.text_secondary))
                        .child(SharedString::from(note.body.trim().to_owned())),
                )
        });
        let label = thread.notes.first().map_or_else(String::new, |first| {
            format!("{}: {}", first.author, kit::first_line(&first.body))
        });
        let url = thread.url.clone();
        self.note()
            .id(ElementId::Name(format!("review-forge-{ix}").into()))
            .debug_selector(move || format!("review-forge-{ix}"))
            .role(Role::Comment)
            .aria_label(SharedString::from(label))
            .child(self.icon(Symbol::ArrowTrianglePull, s.text_muted))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(theme.spacing.xxs))
                    .whitespace_normal()
                    .children(place.map(|place| {
                        kit::tabular(div())
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(place))
                    }))
                    .children(notes),
            )
            .children(url.map(|url| {
                div()
                    .id(ElementId::Name(format!("review-forge-open-{ix}").into()))
                    .debug_selector(move || format!("review-forge-open-{ix}"))
                    .role(Role::Link)
                    .aria_label("Open on the forge")
                    .cursor_pointer()
                    .child(self.icon(Symbol::ArrowUpRight, s.text_muted))
                    .on_click(move |_ev, _w, cx| cx.open_url(&url))
            }))
            .into_any_element()
    }

    /// The pull request the person's comments would go to: the branch's, open, with its
    /// threads read.
    pub(super) fn post_to(&self, cx: &App) -> Option<PostTo> {
        let repo = self.repo(cx)?;
        let git = self.hub.read(cx).git().repo(&repo)?;
        let comments = git.comments.as_ref()?;
        let pull = git.pull.status().filter(|p| p.number == comments.number);
        Some(PostTo {
            number: comments.number,
            forge: git.forge(),
            head: pull.map(|p| p.head_commit.clone()).filter(|h| !h.is_empty()),
        })
    }

    /// The person's own comments: what a post takes. The agent's findings are not theirs to
    /// sign.
    pub(super) fn own_comments(&self) -> Vec<Comment> {
        self.model.comments().iter().filter(|c| c.by.is_none()).cloned().collect()
    }

    /// The foot's post while the person has comments and the branch an open pull request:
    /// "Post 2 comments to #42…", or "Posting…" while one is on its way.
    pub(super) fn post_label(&self, cx: &App) -> Option<String> {
        if self.posting.is_some() {
            return Some("Posting\u{2026}".to_owned());
        }
        let to = self.post_to(cx)?;
        let n = self.own_comments().len();
        (n > 0).then(|| post_words(&to.name(), n))
    }

    /// Post the person's comments to the pull request as one review, with `verdict`.
    pub(super) fn post_review(&mut self, verdict: ReviewVerdict, cx: &mut Context<Self>) {
        self.post_open = false;
        if self.posting.is_some() {
            return;
        }
        let (Some(to), Some(repo)) = (self.post_to(cx), self.repo(cx)) else { return };
        let comments: Vec<Comment> =
            self.own_comments().into_iter().take(REVIEW_NOTES_MAX).collect();
        if comments.is_empty() {
            return;
        }
        let op = GitOp::PullReview {
            number: to.number,
            verdict,
            body: String::new(),
            notes: comments.iter().map(review_note).collect(),
            head: to.head.clone(),
        };
        let asked = self.hub.update(cx, |hub, cx| hub.git_op(&repo, op, cx));
        if let Some(request) = asked {
            self.posting = Some(Posting { request, comments, to: to.name() });
        }
        cx.notify();
    }

    /// The forge answered the post: taken, the comments it took go and what was posted is
    /// said; turned down, they stay and why is said. Whether the comments moved.
    pub(super) fn settle_post(&mut self, cx: &App) -> bool {
        let Some(posting) = &self.posting else { return false };
        let Some(repo) = self.repo(cx) else { return false };
        let said = self.hub.read(cx).git().repo(&repo).and_then(|r| r.said.clone());
        let Some((_, said)) = said.filter(|(request, _)| *request == posting.request) else {
            return false;
        };
        let Some(posting) = self.posting.take() else { return false };
        let (words, failed) = match said {
            Said::Reviewed { posted, .. } => {
                for comment in &posting.comments {
                    if let Some(ix) = self.model.comments().iter().position(|c| c == comment) {
                        self.model.uncomment(ix);
                    }
                }
                (posted_words(&posting.to, posted), false)
            }
            Said::Refused { why } | Said::Failed { said: why } => {
                (format!("Review not posted to {}: {}", posting.to, kit::first_line(&why)), true)
            }
            _ => (format!("Review not posted to {}", posting.to), true),
        };
        self.came = Some(Came { agent: String::new(), words, refused: None, failed });
        true
    }

    /// The post's menu, while open: one row per verdict.
    pub(super) fn post_menu(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.post_open {
            return None;
        }
        let theme = &self.theme;
        let this = cx.entity().downgrade();
        let mut menu = kit::Menu::new();
        for (verdict, slug, words) in VERDICTS {
            let to = this.clone();
            menu.push(kit::MenuItem::new(slug, words, move |_w, cx| {
                let _gone = to.update(cx, |v, cx| v.post_review(verdict, cx));
            }));
        }
        let panel =
            kit::MenuPanel::new("review-post", "Post the review", std::rc::Rc::new(menu), theme, {
                move |window, cx| {
                    let _gone = this.update(cx, |this, cx| {
                        this.post_open = false;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    });
                }
            });
        Some(
            gpui::deferred(gpui::anchored().anchor(gpui::Anchor::BottomRight).child(panel))
                .with_priority(crate::palette::Layer::Submenu.priority())
                .into_any_element(),
        )
    }
}

#[cfg(test)]
impl ReviewView {
    /// What is said above the diff, when something is.
    pub(in crate::review) fn came_words(&self) -> Option<String> {
        self.came.as_ref().map(|c| c.words.clone())
    }
}
