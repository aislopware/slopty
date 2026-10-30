//! The Changes pane's find bar, whitespace toggle and file tree.

use std::ops::Range;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Focusable as _, Hsla, SharedString, Window, div, px};

use super::review::{self, DiffMatch};
use super::{
    Changes, DiffFind, DiffRow, FindInDiff, FindNextInDiff, FindPreviousInDiff, ToggleDiffTree,
    add_color, body_rows, del_color, row_ix,
};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::motion::HoverFade as _;
use crate::settings::widgets::{ActionHint as _, Keys};
use crate::theme::Theme;
use crate::{motion, popover};

/// The changed files' column beside the diff.
const TREE_WIDTH: f32 = 208.0;
const TREE_ROW_HEIGHT: f32 = 24.0;
const TREE_INDENT: f32 = 14.0;
/// Rows of the diff kept above a match the find lands on, clear of the sticky file header.
const FIND_CONTEXT_ROWS: usize = 3;

impl Changes {
    /// Keep the review aids' choices for every diff tab, this run and the next.
    fn keep_prefs(&self, cx: &mut App) {
        let (ignore_whitespace, show_tree) = (self.ignore_whitespace, self.show_tree);
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.diff_ignore_whitespace = ignore_whitespace;
            settings.diff_show_tree = show_tree;
        });
    }

    pub(super) fn toggle_ignore_whitespace(&mut self, cx: &mut Context<Self>) {
        self.ignore_whitespace = !self.ignore_whitespace;
        self.keep_prefs(cx);
        self.sync(cx);
        cx.notify();
    }

    pub(super) fn toggle_tree(&mut self, cx: &mut Context<Self>) {
        self.show_tree = !self.show_tree;
        self.keep_prefs(cx);
        cx.notify();
    }

    /// Show the find bar with the keyboard in it ([`super::FindInDiff`]).
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = &self.find {
            let handle = find.input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            return;
        }
        // "PaletteSearch": ↑↓ and ⏎ stay unbound in the field and reach the card.
        let input = cx.new(|cx| ComposerInput::with_context("Find in diff", "PaletteSearch", cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.refresh_find(cx);
                this.reveal_match();
                cx.notify();
            }
        });
        let handle = input.read(cx).focus_handle(cx);
        self.find = Some(DiffFind { input, focus: cx.focus_handle(), _events: events });
        self.found_for = None;
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Put the find away; the keyboard comes back to the pane on its next frame.
    pub(super) fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.find.take().is_none() {
            return;
        }
        self.found.clear();
        self.found_active = None;
        self.found_for = None;
        self.refocus = true;
        cx.notify();
    }

    /// Match the find's words against the diff as it now reads, keeping the active match where
    /// it still is.
    pub(super) fn refresh_find(&mut self, cx: &Context<Self>) {
        let Some(find) = &self.find else { return };
        let query = find.input.read(cx).text().to_owned();
        let key = self.parsed.as_ref().map(|parsed| parsed.key.clone()).unwrap_or_default();
        if self.found_for.as_ref().is_some_and(|(k, q)| *k == key && *q == query) {
            return;
        }
        let was = self.found_active.and_then(|ix| self.found.get(ix)).cloned();
        self.found = self
            .parsed
            .as_ref()
            .map(|parsed| review::find_matches(&parsed.files, &query))
            .unwrap_or_default();
        self.found_active = if self.found.is_empty() {
            None
        } else {
            was.and_then(|was| self.found.iter().position(|m| *m == was)).or(Some(0))
        };
        self.found_for = Some((key, query));
    }

    /// Move to the next match (`delta` 1) or the previous one (-1), wrapping.
    pub fn step_find(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.find.is_none() {
            return;
        }
        self.refresh_find(cx);
        self.found_active = review::step(self.found_active, self.found.len(), delta);
        self.reveal_match();
        cx.notify();
    }

    /// Scroll the active match into view, opening its file when it is folded.
    fn reveal_match(&mut self) {
        let Some(found) = self.found_active.and_then(|ix| self.found.get(ix)).cloned() else {
            return;
        };
        let file_ix = found.file as usize;
        self.open_file_body(file_ix);
        let Some(range) = self.row_ranges.get(file_ix).cloned() else { return };
        let Some(rows) = self.rows.get(range.clone()) else { return };
        let hit = rows.iter().position(|row| match *row {
            DiffRow::Line { hunk, line, .. } => hunk == found.hunk && line == found.line,
            DiffRow::SplitLine { hunk, left, right, .. } => {
                hunk == found.hunk && (left == Some(found.line) || right == Some(found.line))
            }
            _ => false,
        });
        let Some(hit) = hit.map(|at| at.saturating_add(range.start)) else { return };
        self.list.scroll_to(gpui::ListOffset {
            item_ix: hit.saturating_sub(FIND_CONTEXT_ROWS).max(range.start),
            offset_in_item: px(0.0),
        });
    }

    /// A file folded shut (or folding) opens at once, with no tween.
    fn open_file_body(&mut self, file_ix: usize) {
        let Some(file) = self.parsed.as_ref().and_then(|p| p.files.get(file_ix)).cloned() else {
            return;
        };
        let folding = self
            .row_ranges
            .get(file_ix)
            .and_then(|range| self.rows.get(range.start.saturating_add(1)))
            == Some(&DiffRow::FoldingBody { file: row_ix(file_ix) });
        let collapsed = self.folds.get(&file.path).is_some_and(|fold| fold.collapsed);
        if !collapsed && !folding {
            return;
        }
        let fold = self.folds.entry(file.path.clone()).or_default();
        fold.collapsed = false;
        fold.toggled_at = None;
        let body = body_rows(row_ix(file_ix), &file, self.mode);
        self.replace_file_body(file_ix, &body);
    }

    /// Bring file `file_ix`'s header to the top of the diff.
    pub(super) fn scroll_to_file(&mut self, file_ix: usize, cx: &mut Context<Self>) {
        self.tree_selected = Some(file_ix);
        if let Some(range) = self.row_ranges.get(file_ix) {
            self.list.scroll_to(gpui::ListOffset { item_ix: range.start, offset_in_item: px(0.0) });
        }
        cx.notify();
    }

    /// The find's marks on one line: every match on it, the active one louder.
    pub(super) fn find_marks(
        &self,
        file: u32,
        hunk: u32,
        line: u32,
        theme: &Theme,
    ) -> Vec<(Range<usize>, Hsla)> {
        if self.found.is_empty() {
            return Vec::new();
        }
        let key = |m: &DiffMatch| (m.file, m.hunk, m.line);
        let at = (file, hunk, line);
        let start = self.found.partition_point(|m| key(m) < at);
        let active = self.found_active;
        self.found
            .get(start..)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .take_while(|(_, m)| key(m) == at)
            .map(|(k, m)| {
                let loud = active == Some(start.saturating_add(k));
                (m.range.clone(), theme.warning.opacity(if loud { 0.55 } else { 0.22 }))
            })
            .collect()
    }

    /// The find card's keys, bubbling from its field: ⏎ and ↓ next, ⇧⏎ and ↑ previous, Esc away.
    fn find_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" if event.keystroke.modifiers.shift => self.step_find(-1, cx),
            "enter" | "down" => self.step_find(1, cx),
            "up" => self.step_find(-1, cx),
            "escape" => self.close_find(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // ---- drawing ----

    /// The trailing toolbar's review aids: find, the files' tree and whitespace.
    pub(super) fn render_aid_controls(&self, theme: &Theme, cx: &Context<Self>) -> [AnyElement; 3] {
        let find =
            Self::header_toggle("changes-find", crate::icons::MAGNIFER, self.find.is_some(), theme)
                .action_hint(
                    "Find in diff",
                    Keys::ActionIn(Box::new(FindInDiff), self.focus.clone()),
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    if this.find.is_some() {
                        this.close_find(cx);
                    } else {
                        this.open_find(window, cx);
                    }
                }))
                .into_any_element();
        let tree = Self::header_toggle(
            "changes-tree-toggle",
            crate::icons::FILE_TREE,
            self.show_tree,
            theme,
        )
        .action_hint(
            if self.show_tree { "Hide file tree" } else { "Show file tree" },
            Keys::ActionIn(Box::new(ToggleDiffTree), self.focus.clone()),
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_tree(cx);
        }))
        .into_any_element();
        // The set's pilcrow, not the font's "¶", which sat heavier and off its baseline.
        let whitespace = Self::header_toggle(
            "changes-whitespace",
            crate::icons::PILCROW,
            self.ignore_whitespace,
            theme,
        )
        .hint("Ignore whitespace")
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_ignore_whitespace(cx);
        }))
        .into_any_element();
        [find, tree, whitespace]
    }

    /// The find card floating over the diff's top right.
    pub(super) fn render_find(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let find = self.find.as_ref()?;
        let query = find.input.read(cx).text().to_owned();
        let tally = review::tally(&query, self.found_active, self.found.len());
        let popup = theme.for_popup();
        let any = !self.found.is_empty();
        // The shared find card: the search row's anatomy, frosted, entering as a menu
        // does, its steps naming their keys and dimming with no match to go to (M19).
        let card = popover::find_card(&popup)
            .id("changes-find-card")
            .role(gpui::Role::Search)
            .aria_label("Find in diff")
            .track_focus(&find.focus)
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| this.find_key(event, cx)),
            )
            .occlude()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(crate::typography::ui_rems(13.0))
                    .child(find.input.clone()),
            )
            .child(popover::find_tally(&popup, tally))
            .child(
                popover::find_step(
                    "changes-find-previous",
                    crate::icons::ALT_ARROW_UP,
                    any,
                    &popup,
                )
                .action_hint(
                    "Previous match",
                    Keys::ActionIn(Box::new(FindPreviousInDiff), find.focus.clone()),
                    cx,
                )
                .when(any, |step| {
                    step.on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.step_find(-1, cx);
                    }))
                }),
            )
            .child(
                popover::find_step("changes-find-next", crate::icons::ALT_ARROW_DOWN, any, &popup)
                    .action_hint(
                        "Next match",
                        Keys::ActionIn(Box::new(FindNextInDiff), find.focus.clone()),
                        cx,
                    )
                    .when(any, |step| {
                        step.on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.step_find(1, cx);
                        }))
                    }),
            )
            .child(
                popover::find_step("changes-find-close", crate::icons::CLOSE, true, &popup)
                    .action_hint("Close find", Keys::Fixed("escape"), cx)
                    .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_find(cx);
                    })),
            );
        Some(
            div()
                .absolute()
                .top(px(8.0))
                .right(px(12.0))
                .child(popover::find_card_in("changes-find-card-in", card))
                .into_any_element(),
        )
    }

    /// The changed files as a tree beside the diff; a click brings a file's header to the top.
    pub(super) fn render_tree(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.show_tree {
            return None;
        }
        let parsed = self.parsed.as_ref()?;
        let files = Arc::clone(&parsed.files);
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        let rows = review::diff_tree(&paths);
        let selected = self.tree_selected;
        let rail = popover::rail(self, "changes-tree-rail", theme, cx);
        let list = div()
            .id("changes-tree")
            .role(gpui::Role::Tree)
            .aria_label("Changed files")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.tree_scroll)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.aid_rail.set_list_hovered(*hovered) {
                    cx.notify();
                }
            }))
            .py(px(4.0))
            .children(rows.into_iter().map(|row| {
                let key = SharedString::from(format!("changes-tree-row:{}", row.path));
                let file = row.file;
                let on = file.is_some() && file == selected;
                let identity = match file {
                    Some(_) => crate::file_icons::FileIconIdentity::file(&row.name),
                    None => crate::file_icons::FileIconIdentity::directory(&row.name, true),
                };
                let icon = crate::file_icons::icon(identity, theme.appearance).size(px(14.0));
                let counts = file.and_then(|ix| files.get(ix)).map(|diff| {
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(4.0))
                        // Counts in the file headers' tabular figures (item 18).
                        .font_features(crate::typography::tabular())
                        .text_size(crate::typography::ui_rems(11.0))
                        .when(diff.additions > 0, |el| {
                            el.child(
                                div()
                                    .text_color(add_color(theme))
                                    .child(SharedString::from(format!("+{}", diff.additions))),
                            )
                        })
                        .when(diff.deletions > 0, |el| {
                            el.child(
                                div()
                                    .text_color(del_color(theme))
                                    .child(SharedString::from(format!("−{}", diff.deletions))),
                            )
                        })
                });
                div()
                    .id(gpui::ElementId::Name(key.clone()))
                    .role(gpui::Role::TreeItem)
                    .aria_label(SharedString::from(row.path.clone()))
                    .h(px(TREE_ROW_HEIGHT))
                    .mx(px(4.0))
                    .pl(px(f32::from(u16::try_from(row.depth).unwrap_or(u16::MAX))
                        .mul_add(TREE_INDENT, 6.0)))
                    .pr(px(6.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(popover::MENU_ITEM_RADIUS))
                    .map(|el| {
                        if on {
                            el.bg(crate::theme::card_selected_bg())
                        } else if file.is_some() {
                            el.cursor_pointer()
                                .bg(motion::hover_blend(
                                    &key,
                                    crate::theme::ink(0.0),
                                    crate::theme::ink(0.05),
                                ))
                                .hover_fade(key.clone())
                        } else {
                            el
                        }
                    })
                    .when_some(file, |el, ix| {
                        // A keyboard stop with the shell's ring, inside the tree (M8).
                        el.map(|el| crate::surface_chrome::key_stop_inset(el, theme)).on_click(
                            cx.listener(move |this, _, _, cx| this.scroll_to_file(ix, cx)),
                        )
                    })
                    .child(icon)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(if on || file.is_none() {
                                theme.text_dim
                            } else {
                                theme.text_muted
                            })
                            .child(SharedString::from(row.name)),
                    )
                    .children(counts)
            }));
        Some(
            div()
                .relative()
                .w(px(TREE_WIDTH))
                .flex_none()
                .h_full()
                .border_r_1()
                .border_color(crate::theme::hairline(0.06))
                .child(popover::faded_menu_list(&self.tree_scroll, list))
                .children(rail)
                .into_any_element(),
        )
    }
}

impl popover::ScrollRailHost for Changes {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        &mut self.aid_rail
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.show_tree.then(|| self.tree_scroll.clone())
    }
}
