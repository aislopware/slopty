//! The files the latest turn changed, as a card under its answer once it settled, as
//! `MonoCode`'s session review stands (`docs/decisions/ui.md`, "The latest turn ends in the
//! files it changed").
//!
//! Its head says how many files and lines, with Undo, Keep and Review. Under it are the files,
//! the first [`SHOWN`] and a way to the rest, each opening the review. Keep takes every file
//! into what the person has kept (`Intent::Keep`), and Undo puts each back as it was before the
//! turn (`Intent::Revert`), both by the turn's own review (`ReviewScope::Turn`), asked once as
//! the card shows. Either done, the card goes; a file the worker would not keep or put back is
//! said in the tray, as the review tile's are. Without the worker's snapshots there is no turn
//! review, so the card offers only Review.

use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_proto::thread::wire::{Intent, Pick, Review, ReviewScope};
use slopty_proto::thread::{Cap, TurnId};

use super::{TOOL_ROW, ThreadView, ThreadViewEvent};
use crate::colors::hsla;
use crate::conversation::thread::activity;
use crate::conversation::thread::rows::Row;
use crate::icons::Symbol;
use crate::kit::{self, ButtonKind};

/// How many files the card lists before the way to the rest.
pub(super) const SHOWN: usize = 3;

/// One changed file as the card lists it.
struct Listed {
    /// As a person reads it ([`shown_path`]).
    path: String,
    added: u32,
    removed: u32,
}

impl ThreadView {
    /// The turn whose changed files the rows end in, if they do.
    fn changes_turn(&self) -> Option<TurnId> {
        self.rows.iter().find_map(|r| match r {
            Row::Changes { turn } => Some(*turn),
            _ => None,
        })
    }

    /// The card's turn review, once it came: what Keep and Undo act on.
    pub(super) fn turn_review(&self, turn: TurnId, cx: &App) -> Option<Arc<Review>> {
        let hub = self.hub.read(cx);
        hub.review(self.thread, &ReviewScope::Turn(turn)).filter(|r| r.absent.is_none()).cloned()
    }

    /// Ask for the turn review of the card on show, once, where the worker snapshots the tree.
    pub(super) fn ask_changes(&mut self, cx: &mut Context<Self>) {
        let Some(turn) = self.changes_turn() else { return };
        if self.changes_asked == Some(turn) {
            return;
        }
        let snapshots = self.state(cx).is_some_and(|st| st.meta.can(Cap::SNAPSHOTS));
        if !snapshots || !self.hub.read(cx).linked() {
            return;
        }
        self.changes_asked = Some(turn);
        let thread = self.thread;
        self.hub.update(cx, |hub, cx| hub.ask_review(thread, ReviewScope::Turn(turn), cx));
    }

    /// Keep every file the turn changed, or put each back: the card is done either way.
    pub(super) fn settle_changes(&mut self, turn: TurnId, keep: bool, cx: &mut Context<Self>) {
        let Some(review) = self.turn_review(turn, cx) else { return };
        for file in &review.files {
            let pick = Pick {
                path: file.path.clone(),
                from: file.from.clone(),
                stamp: file.to.clone(),
                hunks: Vec::new(),
                old_path: file.old_path.clone(),
            };
            let intent = if keep { Intent::Keep(pick) } else { Intent::Revert(pick) };
            let _id = self.intent(intent, cx);
        }
        self.kept.insert(turn);
        self.rebuild(cx);
    }

    /// The files the card lists: the turn review's where it came, else the turn's own edits.
    fn listed(&self, turn: TurnId, cx: &App) -> Vec<Listed> {
        let cwd = self.state(cx).map(|st| st.meta.cwd.clone()).unwrap_or_default();
        match self.turn_review(turn, cx) {
            Some(review) => review
                .files
                .iter()
                .map(|f| Listed {
                    path: f.path.clone(),
                    added: f.patch.added,
                    removed: f.patch.removed,
                })
                .collect(),
            None => self
                .state(cx)
                .map(activity::edited)
                .unwrap_or_default()
                .into_iter()
                .map(|e| Listed {
                    path: shown_path(&e.path, &cwd),
                    added: e.added,
                    removed: e.removed,
                })
                .collect(),
        }
    }

    /// The card of the files `turn` changed: its head with Undo, Keep and Review, then the
    /// files, each opening the review.
    pub(super) fn changes_card(&self, turn: TurnId, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let files = self.listed(turn, cx);
        let (added, removed) = files.iter().fold((0_u32, 0_u32), |(a, r), f| {
            (a.saturating_add(f.added), r.saturating_add(f.removed))
        });
        let words = match files.len() {
            1 => "Changed 1 file".to_owned(),
            n => format!("Changed {n} files"),
        };
        let thread = self.thread;
        let acts = self.turn_review(turn, cx).is_some_and(|r| !r.files.is_empty());
        let head = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .px(px(theme.spacing.sm))
            .min_h(px(theme.density.row))
            .child(self.icon(Symbol::Pencil, s.text_muted))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .text_size(px(theme.typography.small()))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(words.clone())),
                    )
                    .children(kit::changes(theme, added, removed)),
            )
            .when(acts, |el| {
                el.child(self.button("changes-undo", UNDO, ButtonKind::Ghost).on_click(
                    cx.listener(move |this, _ev, _w, cx| this.settle_changes(turn, false, cx)),
                ))
                .child(
                    self.button("changes-keep", KEEP, ButtonKind::Ghost).on_click(
                        cx.listener(move |this, _ev, _w, cx| this.settle_changes(turn, true, cx)),
                    ),
                )
            })
            .child(self.button("changes-review", REVIEW, ButtonKind::Secondary).on_click(
                cx.listener(move |_this, _ev, _w, cx| cx.emit(ThreadViewEvent::Review { thread })),
            ));
        let more = files.len().saturating_sub(SHOWN);
        let open = self.changes_open;
        let shown = if open { files.len() } else { SHOWN };
        let rows = files.iter().take(shown).enumerate().map(|(ix, file)| {
            let name = file.path.rsplit('/').next().unwrap_or(&file.path).to_owned();
            crate::a11y::tab_stop(
                div()
                    .id(("changes-file", ix))
                    .debug_selector(move || format!("changes-file-{ix}"))
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!("Review {}", file.path)))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .px(px(theme.spacing.sm))
                    .min_h(px(TOOL_ROW))
                    .cursor_pointer()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                    .child(Self::slot().child(crate::icons::symbol(
                        theme,
                        crate::icons::file_mark(&name),
                        px(theme.typography.icon()),
                        hsla(s.text_muted),
                    )))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(self.mono())
                            .child(SharedString::from(file.path.clone())),
                    )
                    .children(kit::changes(theme, file.added, file.removed))
                    .on_click(cx.listener(move |_this, _ev, _w, cx| {
                        cx.emit(ThreadViewEvent::Review { thread });
                    })),
                s.focus,
            )
        });
        let toggle = (more > 0).then(|| {
            let said = if open {
                "Show fewer files".to_owned()
            } else if more == 1 {
                "Show 1 more file".to_owned()
            } else {
                format!("Show {more} more files")
            };
            div()
                .id("changes-more")
                .debug_selector(|| "changes-more".to_owned())
                .role(Role::Button)
                .aria_label(SharedString::from(said.clone()))
                .aria_expanded(open)
                .w_full()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.sm))
                .min_h(px(TOOL_ROW))
                .cursor_pointer()
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .hover(move |el| el.text_color(hsla(s.text_secondary)))
                .child(Self::slot().child(self.chevron("changes-more-chevron".to_owned(), open)))
                .child(SharedString::from(said))
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.changes_open = !this.changes_open;
                    this.rebuild(cx);
                }))
        });
        let said = match kit::changes(theme, added, removed) {
            Some(_) => format!("{words}, {added} added, {removed} removed"),
            None => words,
        };
        kit::inset(div(), theme)
            .id(ElementId::Name(format!("changes-{}", turn.0).into()))
            .debug_selector(move || format!("changes-{}", turn.0))
            .role(Role::Group)
            .aria_label(SharedString::from(said))
            .w_full()
            .flex()
            .flex_col()
            .py(px(theme.spacing.xxs))
            .rounded(px(theme.radii.md))
            .border(kit::HAIR)
            .border_color(hsla(s.border))
            .overflow_hidden()
            .child(head)
            .children(rows)
            .children(toggle)
            .into_any_element()
    }
}

/// A file the turn edited as the card names it: under the agent's folder, relative to it;
/// anywhere else by its name alone, since a whole path from the root reads as noise beside the
/// others.
fn shown_path(path: &str, cwd: &str) -> String {
    let tidy = super::tools::tidy(path, cwd);
    if tidy.starts_with('/') {
        tidy.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(&tidy).to_owned()
    } else {
        tidy
    }
}

/// The card's way to put the turn's files back as they were.
pub(super) const UNDO: &str = "Undo";

/// The card's way to keep the turn's files as they are.
pub(super) const KEEP: &str = "Keep";

/// The card's way to the whole review.
pub(super) const REVIEW: &str = "Review";

#[cfg(test)]
mod tests {
    /// Under the folder a path is relative; outside it, the file's name alone.
    #[test]
    fn a_path_reads_from_the_folder_or_by_its_name() {
        assert_eq!(super::shown_path("/w/src/a.rs", "/w"), "src/a.rs");
        assert_eq!(super::shown_path("/work/notes.md", "/tmp/e2e/project"), "notes.md");
    }
}
