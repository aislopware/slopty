//! A file the review's line budget left without hunks, shown for what it is and read whole on
//! the person's press.
//!
//! The worker carries at most `REVIEW_LINES` diff lines in one review, spent in the order the
//! files are read, so a file past what was left comes with its counts and how many lines it has
//! (`Patch::clipped_lines`) but no hunks. Its row says so, and a press asks the worker for that
//! file alone by the blobs the review named (`GitOp::FileDiff`), through the repository the
//! review is of, a thread's as a folder's. The hunks that come take the place of the empty row,
//! cut as the review cuts every file, so its folds, comments and picks work as on any other.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_proto::git::GitOp;

use super::{ReviewView, Row};
use crate::colors::hsla;
use crate::conversation::diff;
use crate::conversation::thread::git::Whole;
use crate::icons::{IconSize, Symbol};

/// What the row says of a file of `lines` lines left out: how many, and why.
pub(super) fn left_out(lines: u32) -> String {
    let count = if lines == 1 { "1 line".to_owned() } else { format!("{lines} lines") };
    format!("{count} of changes, past what one review shows at once")
}

/// The press that reads it whole.
pub(super) const SHOW: &str = "Show this file's changes";
/// While it is read.
pub(super) const READING: &str = "Reading this file's changes\u{2026}";

/// Where a file left out stands.
enum Stands {
    /// Not asked, or the tile cannot ask (no repository known yet).
    Unasked { can_ask: bool },
    /// Asked; the answer is on its way.
    Reading,
    /// It could not be read, in the worker's words.
    Failed(String),
}

impl ReviewView {
    /// The op that reads the file at `at` whole, when it was left out and has a side to read.
    fn whole_op(&self, at: usize) -> Option<GitOp> {
        let file = self.model.file(at)?;
        (file.patch.clipped_lines > 0 && file.is_text())
            .then(|| GitOp::FileDiff { from: file.from.clone(), to: file.to.clone() })
    }

    fn stands(&self, at: usize, cx: &gpui::App) -> Stands {
        let (Some(op), Some(repo)) = (self.whole_op(at), self.repo(cx)) else {
            return Stands::Unasked { can_ask: false };
        };
        let book = self.hub.read(cx).git();
        if book.asking(&repo, &op) {
            return Stands::Reading;
        }
        let GitOp::FileDiff { from, to } = op else { return Stands::Unasked { can_ask: true } };
        match book.repo(&repo).and_then(|r| r.whole.get(&(from, to))) {
            Some(Whole::Failed(why)) => Stands::Failed(why.clone()),
            Some(Whole::Came(_)) | None => Stands::Unasked { can_ask: true },
        }
    }

    /// Ask for the file at `at` whole.
    fn ask_whole(&self, at: usize, cx: &mut Context<Self>) {
        let (Some(op), Some(repo)) = (self.whole_op(at), self.repo(cx)) else { return };
        let _asked = self.hub.update(cx, |hub, cx| hub.git_op(&repo, op, cx));
        self.redraw_bare(at);
        cx.notify();
    }

    /// Take the hunks of every file left out that came whole: they take the place of its
    /// empty row. Whether any did.
    pub(super) fn take_whole(&mut self, cx: &gpui::App) -> bool {
        let Some(repo) = self.repo(cx) else { return false };
        let hub = self.hub.read(cx);
        let Some(book) = hub.git().repo(&repo) else { return false };
        let Some(review) = self.model.review().cloned() else { return false };
        let mut took = false;
        for (at, file) in review.files.iter().enumerate() {
            if file.patch.clipped_lines == 0 || self.blocks.get(&at).is_some_and(|b| !b.is_empty())
            {
                continue;
            }
            if let Some(Whole::Came(patch)) = book.whole.get(&(file.from.clone(), file.to.clone()))
            {
                self.blocks.insert(at, diff::thread_blocks(&file.path, patch));
                took = true;
            }
        }
        took
    }

    /// Draw the empty row of the file at `at` again: what it says moved.
    fn redraw_bare(&self, at: usize) {
        if let Some(ix) = self.rows.iter().position(|r| *r == Row::Bare(at)) {
            self.list.remeasure_items(ix..ix.saturating_add(1));
        }
    }

    /// Draw the row of every file left out again: its reading moved on.
    pub(super) fn redraw_left_out(&self) {
        for (ix, row) in self.rows.iter().enumerate() {
            if let Row::Bare(at) = *row
                && self.model.file(at).is_some_and(|f| f.patch.clipped_lines > 0)
            {
                self.list.remeasure_items(ix..ix.saturating_add(1));
            }
        }
    }

    /// The row of a file left out: how many lines it has and why it is not shown, and the
    /// press that reads it whole; while it is read, or why it could not be.
    pub(super) fn whole_row(&self, at: usize, lines: u32, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let stands = self.stands(at, cx);
        let (press, said) = match &stands {
            Stands::Unasked { can_ask } => (can_ask.then_some(SHOW), None),
            Stands::Reading => (None, Some(READING.to_owned())),
            Stands::Failed(why) => (Some(SHOW), Some(format!("It could not be read: {why}"))),
        };
        let id = format!("review-whole-{at}");
        div()
            .debug_selector(move || format!("review-left-out-{at}"))
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.sm))
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xxs))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(left_out(lines)))
            .children(said.map(|said| {
                div()
                    .id(ElementId::Name(format!("review-whole-said-{at}").into()))
                    .debug_selector(move || format!("review-whole-said-{at}"))
                    .role(Role::Status)
                    .text_color(hsla(s.text_secondary))
                    .child(said)
            }))
            .when_some(press, |el, press| {
                let selector = id.clone();
                el.child(
                    div()
                        .id(ElementId::Name(id.into()))
                        .debug_selector(move || selector)
                        .role(Role::Button)
                        .aria_label(SharedString::from(press))
                        .flex()
                        .items_center()
                        .gap(px(theme.spacing.xs))
                        .cursor_pointer()
                        .text_color(hsla(s.text_secondary))
                        .hover(move |el| el.text_color(hsla(s.text)))
                        .child(
                            crate::icons::icon(
                                theme,
                                Symbol::Unfold,
                                IconSize::Inline,
                                hsla(s.text_muted),
                            )
                            .size(px(theme.typography.icon())),
                        )
                        .child(press)
                        .on_click(cx.listener(move |this, _ev, _w, cx| this.ask_whole(at, cx))),
                )
            })
            .into_any_element()
    }
}
