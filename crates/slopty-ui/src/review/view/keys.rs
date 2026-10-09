//! The review by keyboard: where it stands, how it walks, and what its keys do there.
//!
//! The keyboard stands on a change (a hunk's head) or on a file's head, never on a line. ↓ and
//! ↑ walk the changes in the diff's order; a file folded to its head, or one with no lines to
//! show, is one stop. With ⇧ they walk the files' heads. Keep and put back act on what the
//! keyboard stands on (the file, for a file of one change) and step on to the next, so a run of
//! ⌘Y reads a review down. `c` opens a comment on the whole change; `v` marks its file viewed,
//! folds it and steps to the next file. Viewed files are the person's own marks, kept by the
//! file's path and the blob it shows: a file that changes again is not viewed.

use std::rc::Rc;

use gpui::accesskit::{Role, Toggled};
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::{ReviewView, Row, Span};
use crate::colors::hsla;
use crate::icons::Symbol;
use crate::kit;

/// Where the keyboard stands: a file's head (`hunk` none), or one of its changes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(in crate::review) struct Cursor {
    /// The file, by its place in the review.
    pub at: usize,
    /// The change, by its place in the file.
    pub hunk: Option<usize>,
}

impl ReviewView {
    /// The stops the keyboard walks, in the diff's order: each change, and the head of a file
    /// that shows none; with `files`, every file's head.
    pub(super) fn stops(&self, files: bool) -> Vec<Cursor> {
        let mut out: Vec<Cursor> = Vec::new();
        for (ix, row) in self.rows.iter().enumerate() {
            match *row {
                Row::File(at) => {
                    let shows_changes = matches!(self.rows.get(ix.saturating_add(1)), Some(Row::Gap(a, _) | Row::Hunk(a, _)) if *a == at);
                    if files || !shows_changes {
                        out.push(Cursor { at, hunk: None });
                    }
                }
                Row::Hunk(at, hunk) if !files => out.push(Cursor { at, hunk: Some(hunk) }),
                _ => {}
            }
        }
        out
    }

    /// Step `by` stops (of files, with `files`) from where the keyboard stands, and bring the
    /// stop into view. From nowhere, ↓ goes to the first and ↑ to the last.
    pub(super) fn step(&mut self, by: isize, files: bool, cx: &mut Context<Self>) {
        let stops = self.stops(files);
        if stops.is_empty() {
            return;
        }
        let here = self.cursor.and_then(|c| {
            let c = if files { Cursor { hunk: None, ..c } } else { c };
            stops.iter().position(|s| *s == c)
        });
        let last = stops.len().saturating_sub(1);
        let next = match here {
            Some(at) if by < 0 => at.saturating_sub(by.unsigned_abs()),
            Some(at) => at.saturating_add(by.unsigned_abs()).min(last),
            None if by < 0 => last,
            None => 0,
        };
        self.stand_on(stops.get(next).copied(), cx);
    }

    /// Stand on `cursor` and bring its row into view.
    pub(super) fn stand_on(&mut self, cursor: Option<Cursor>, cx: &mut Context<Self>) {
        self.cursor = cursor;
        if let Some(row) = cursor.and_then(|c| self.row_of(c)) {
            self.list.scroll_to_reveal_item(row);
        }
        cx.notify();
    }

    /// The row that draws `cursor`.
    pub(super) fn row_of(&self, cursor: Cursor) -> Option<usize> {
        let want = match cursor.hunk {
            Some(hunk) => Row::Hunk(cursor.at, hunk),
            None => Row::File(cursor.at),
        };
        self.rows.iter().position(|r| *r == want)
    }

    /// Whether the keyboard stands on `cursor`'s row.
    pub(super) fn stands_on(&self, at: usize, hunk: Option<usize>) -> bool {
        self.cursor == Some(Cursor { at, hunk })
    }

    /// Keep (or with `keep` false, put back) what the keyboard stands on: a change, or its
    /// whole file where that is its one change. Then stand on the next stop.
    pub(super) fn pick_by_key(&mut self, keep: bool, cx: &mut Context<Self>) {
        let Some(cursor) = self.cursor else {
            self.step(1, false, cx);
            return;
        };
        let one = self.blocks.get(&cursor.at).is_none_or(|b| b.len() < 2);
        let hunk = cursor.hunk.filter(|_| !one);
        self.pick(cursor.at, hunk, keep, cx);
        self.step(1, false, cx);
    }

    /// Open a comment on every line of the change the keyboard stands on: a file's head
    /// comments on its first change.
    pub(super) fn comment_by_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cursor) = self.cursor else { return };
        let hunk = cursor.hunk.unwrap_or(0);
        let Some(lines) =
            self.blocks.get(&cursor.at).and_then(|b| b.get(hunk)).map(|b| b.lines.len())
        else {
            return;
        };
        if lines == 0 {
            return;
        }
        let span = Span { at: cursor.at, hunk, from: 0, to: lines.saturating_sub(1) };
        self.start_comment(span, window, cx);
    }

    /// The file's mark as viewed: its path and the blob it shows.
    pub(super) fn viewed_key(&self, at: usize) -> Option<(String, Option<String>)> {
        self.model.file(at).map(|f| (f.path.clone(), f.to.clone()))
    }

    /// Whether the file at `at` is marked viewed, as it is now.
    pub(super) fn is_viewed(&self, at: usize) -> bool {
        self.viewed_key(at).is_some_and(|k| self.viewed.contains(&k))
    }

    /// Mark the file at `at` viewed, folding it to its head, or not viewed, opening it.
    pub(super) fn toggle_viewed(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(key) = self.viewed_key(at) else { return };
        let path = key.0.clone();
        if self.viewed.remove(&key) {
            self.folded.remove(&path);
        } else {
            self.viewed.insert(key);
            self.folded.insert(path);
        }
        self.rebuild();
        cx.notify();
    }

    /// `v`: the keyboard's file marked viewed (or not), and the keyboard on the next file.
    pub(super) fn viewed_by_key(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.cursor.map(|c| c.at) else { return };
        let now = !self.is_viewed(at);
        self.toggle_viewed(at, cx);
        self.cursor = Some(Cursor { at, hunk: None });
        if now {
            self.step(1, true, cx);
        } else {
            self.stand_on(self.cursor, cx);
        }
    }

    /// How many files are marked viewed.
    #[cfg(test)]
    pub(in crate::review) fn viewed_count(&self) -> usize {
        self.viewed.len()
    }

    /// The file the keyboard stands in.
    #[cfg(test)]
    pub(in crate::review) fn cursor_file(&self) -> Option<usize> {
        self.cursor.map(|c| c.at)
    }

    /// What the person left here: the comments not yet sent and the files marked viewed.
    #[must_use]
    pub fn left(&self) -> crate::workspace::ReviewDraft {
        let mut viewed: Vec<(String, Option<String>)> = self.viewed.iter().cloned().collect();
        viewed.sort();
        crate::workspace::ReviewDraft { comments: self.model.comments().to_vec(), viewed }
    }

    /// Take back what was left here before the app last went.
    pub fn take_back(&mut self, left: crate::workspace::ReviewDraft, cx: &mut Context<Self>) {
        self.model.take_back(left.comments);
        for (path, blob) in left.viewed {
            self.folded.insert(path.clone());
            self.viewed.insert((path, blob));
        }
        self.left = self.left_mark();
        self.rebuild();
        cx.notify();
    }

    /// A mark of what is left here, to tell when it changed.
    pub(super) fn left_mark(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut h = std::hash::DefaultHasher::new();
        self.model.comments().hash(&mut h);
        self.viewed.iter().collect::<std::collections::BTreeSet<_>>().hash(&mut h);
        h.finish()
    }

    // ----- drawing ---------------------------------------------------------------------

    /// "Viewed" on a file's head: a tick box and the word, pressed as one, as a forge's diff
    /// has it.
    pub(super) fn viewed_box(&self, at: usize, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let on = self.is_viewed(at);
        let selector = format!("review-viewed-{at}");
        crate::a11y::tab_stop(
            div()
                .id(ElementId::Name(selector.clone().into()))
                .debug_selector(move || selector)
                .role(Role::CheckBox)
                .aria_label("Viewed")
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .text_color(hsla(if on { s.text_secondary } else { s.text_muted }))
                .cursor_pointer()
                .hover(move |el| el.text_color(hsla(s.text)))
                .child(kit::tick_box(theme, on))
                .child("Viewed")
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    cx.stop_propagation();
                    this.toggle_viewed(at, cx);
                })),
            s.focus,
        )
        .into_any_element()
    }

    /// On a tile too narrow for the list of files beside the diff: a button in the scope bar,
    /// and while it is open, the files as a menu, each going to its file.
    pub(super) fn files_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let review = self.model.review().filter(|r| !r.files.is_empty())?;
        if self.room().is_wide() {
            return None;
        }
        let theme = &self.theme;
        let count = review.files.len();
        let label = kit::count(count as u64, "file", "files");
        let button = kit::icon_button(theme, "review-files-button", Symbol::Doc, "Files")
            .aria_label(SharedString::from(format!("Go to a file, {label}")))
            .aria_expanded(self.files_open)
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.files_open = !this.files_open;
                cx.notify();
            }));
        let this = cx.entity().downgrade();
        let menu = self.files_open.then(|| {
            let mut menu = kit::Menu::new();
            for listed in self.model.listed() {
                let Some(file) = self.model.file(listed.at) else { continue };
                let at = listed.at;
                let to = this.clone();
                let detail = if self.is_viewed(at) {
                    Some("Viewed".to_owned())
                } else {
                    kit::changes_text(file.patch.added, file.patch.removed)
                };
                let item =
                    kit::MenuItem::new(format!("file-{at}"), file.path.clone(), move |_w, cx| {
                        let _gone = to.update(cx, |this, cx| {
                            this.files_open = false;
                            this.reveal(at, cx);
                            this.stand_on(Some(Cursor { at, hunk: None }), cx);
                        });
                    });
                menu.push(match detail {
                    Some(detail) => item.detail(detail),
                    None => item,
                });
            }
            let panel = kit::MenuPanel::new(
                "review-files",
                "Files",
                Rc::new(menu),
                theme,
                move |window, cx| {
                    let _gone = this.update(cx, |this, cx| {
                        this.files_open = false;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    });
                },
            );
            gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopLeft).child(panel))
                .with_priority(crate::palette::Layer::Submenu.priority())
        });
        Some(div().relative().flex_none().child(button).children(menu).into_any_element())
    }
}
