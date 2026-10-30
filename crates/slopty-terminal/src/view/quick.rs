//! Quick select, after kitty's hints and `WezTerm`'s `QuickSelect` (`docs/decisions/terminal.md`,
//! "One chord labels what is on screen"): ⇧⌘Space labels every link, file path and commit hash
//! the grid shows, and typing a label opens it, a path in Files
//! ([`TerminalViewEvent::ViewFile`]), a hash in Changes ([`TerminalViewEvent::ViewCommit`]) and
//! a link in the Browser ([`TerminalViewEvent::Browse`]); ⇧ with the label copies it instead.
//! Esc, or any key that is no label, leaves it, and none of its keys reaches the program.

use std::collections::VecDeque;

use gpui::{
    Context, InteractiveElement as _, IntoElement as _, Keystroke, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_grid::LineIndex;
use slopty_shell::live::chrome;

use super::{LINK_ROWS, QuickSelect, TerminalView, TerminalViewEvent};
use crate::url::{self, Found};

/// The letters labels are made of, the home row first: `WezTerm`'s `QuickSelect` alphabet.
pub(super) const ALPHABET: &str = "asdfqwerzxcvjklmiuopghtybn";

/// Labels for `count` targets made of `alphabet`'s letters, the shortest first and none the
/// start of another, so a label is chosen the moment it is typed: each letter alone while
/// there are letters enough, then the first letters given up for two-letter labels after them.
pub(super) fn labels(count: usize, alphabet: &str) -> Vec<String> {
    let letters: Vec<char> = alphabet.chars().collect();
    if letters.len() < 2 {
        return letters.iter().take(count).map(char::to_string).collect();
    }
    let mut out: VecDeque<String> = letters.iter().map(char::to_string).collect();
    while out.len() < count {
        let Some(prefix) = out.pop_front() else { break };
        out.extend(letters.iter().map(|letter| format!("{prefix}{letter}")));
    }
    out.truncate(count);
    out.into()
}

/// One label on screen: what it says, where it is drawn (screen row and column, inside the
/// grid) and what choosing it does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Label {
    text: String,
    row: u16,
    col: u16,
    found: Found,
}

/// Quick select while it is on.
#[derive(Debug)]
pub(super) struct Quick {
    /// The labels, the one for the target nearest the bottom first.
    labels: Vec<Label>,
    /// What is typed of a label so far.
    typed: String,
}

/// Where a label for a target starting at `(row, col)` is drawn on a grid `cols` wide: at the
/// target's first cell, moved left to end at the grid's edge if it would run past it; `None`
/// for a label wider than the grid.
pub(super) fn label_col(col: u16, width: usize, cols: u16) -> Option<u16> {
    let width = u16::try_from(width).ok()?;
    let most = cols.checked_sub(width)?;
    Some(col.min(most))
}

impl TerminalView {
    /// Every target the grid shows, with the screen cell it starts at (its first one on
    /// screen, when it starts above), the one nearest the bottom first.
    fn on_screen(&self) -> Vec<((u16, u16), Found)> {
        let rows = u64::from(self.state.size().rows);
        let top = self.state.index_at_row(0);
        let bottom = top.0.saturating_add(rows);
        let mut out = Vec::new();
        let mut index = top;
        while index.0 < bottom {
            // Rows above the oldest line the worker keeps hold nothing.
            if self.state.line(index).is_none() {
                index = index.next();
                continue;
            }
            let (first, last) = self.logical_line(index, LINK_ROWS);
            let lines: Vec<&slopty_grid::Line> =
                (first.0..=last.0).map_while(|i| self.state.line(LineIndex(i))).collect();
            for target in url::targets(&lines) {
                let line = |row: usize| first.0.saturating_add(u64::try_from(row).unwrap_or(0));
                let (start, end) = (line(target.start.0), line(target.end.0));
                if end < top.0 || start >= bottom {
                    continue;
                }
                let (row, col) = if start < top.0 { (top.0, 0) } else { (start, target.start.1) };
                let row = u16::try_from(row.saturating_sub(top.0)).unwrap_or(u16::MAX);
                out.push(((row, col), target.found));
            }
            index = last.next();
        }
        out.reverse();
        out
    }

    /// ⇧⌘Space: label what the grid shows, or leave quick select.
    pub fn quick_select(
        &mut self,
        _: &QuickSelect,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.quick.take().is_none() {
            let cols = self.state.size().cols;
            let targets = self.on_screen();
            let texts = labels(targets.len(), ALPHABET);
            let labels = targets
                .into_iter()
                .zip(texts)
                .filter_map(|(((row, col), found), text)| {
                    let col = label_col(col, text.chars().count(), cols)?;
                    Some(Label { text, row, col, found })
                })
                .collect();
            self.quick = Some(Quick { labels, typed: String::new() });
        }
        cx.notify();
    }

    /// The labels on show and where, for the test that reads them.
    #[cfg(test)]
    pub(super) fn quick_labels(&self) -> Option<Vec<(String, u16, u16)>> {
        self.quick.as_ref().map(|quick| {
            quick.labels.iter().map(|label| (label.text.clone(), label.row, label.col)).collect()
        })
    }

    /// A key while quick select is on: a letter that goes on to a label is kept, one that
    /// completes it chooses it (⇧ copies), ⌫ takes the last back, and anything else leaves.
    /// Every key is quick select's: none reaches the program.
    pub(super) fn quick_key(&mut self, key: &Keystroke, cx: &mut Context<Self>) {
        let Some(quick) = &mut self.quick else { return };
        let mods = key.modifiers;
        let letter = key.key.chars().next().filter(|c| {
            key.key.chars().count() == 1
                && c.is_ascii_lowercase()
                && !(mods.control || mods.alt || mods.platform || mods.function)
        });
        match (key.key.as_str(), letter) {
            ("backspace", _) if !quick.typed.is_empty() => {
                quick.typed.pop();
            }
            (_, Some(letter)) => {
                let typed = format!("{}{letter}", quick.typed);
                if let Some(label) = quick.labels.iter().find(|label| label.text == typed) {
                    let found = label.found.clone();
                    self.quick = None;
                    Self::choose(found, mods.shift, cx);
                } else if quick.labels.iter().any(|label| label.text.starts_with(&typed)) {
                    quick.typed = typed;
                }
            }
            _ => self.quick = None,
        }
        cx.notify();
    }

    /// A label chosen: open what it names where it belongs, or copy it.
    fn choose(found: Found, copy: bool, cx: &mut Context<Self>) {
        tracing::info!(?found, copy, "quick select");
        if copy {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(found.text()));
            return;
        }
        match found {
            Found::Path { path, line } => cx.emit(TerminalViewEvent::ViewFile { path, line }),
            Found::Commit(hash) => cx.emit(TerminalViewEvent::ViewCommit { hash }),
            Found::Link(link) => match url::destination(&link) {
                url::Destination::File(path) => {
                    cx.emit(TerminalViewEvent::ViewFile { path, line: None });
                }
                url::Destination::WorkerPage(url) => cx.emit(TerminalViewEvent::OpenPage { url }),
                url::Destination::Here(url) => cx.emit(TerminalViewEvent::Browse { url }),
            },
        }
    }

    /// The labels over the grid, each on its target's first cell in the terminal's own face:
    /// the accent behind it, what is typed of it faded, and the labels that no longer match
    /// gone. The grid under them is dimmed while quick select is on (see `render`).
    pub(super) fn render_quick(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let quick = self.quick.as_ref()?;
        let m = self.metrics?;
        let shell = chrome::theme(cx);
        let pad = self.pad();
        let family = self.font_family.clone().unwrap_or_else(|| {
            self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
        });
        let labels =
            quick.labels.iter().filter(|label| label.text.starts_with(&quick.typed)).map(|label| {
                let rest = label.text.get(quick.typed.len()..).unwrap_or_default().to_owned();
                let width = u16::try_from(label.text.chars().count()).unwrap_or(u16::MAX);
                div()
                    .absolute()
                    .left(px(pad) + m.cell_width * f32::from(label.col))
                    .top(px(pad) + m.line_height * f32::from(label.row))
                    .w(m.cell_width * f32::from(width))
                    .h(m.line_height)
                    .flex()
                    .items_center()
                    .bg(shell.accent)
                    .text_color(shell.on_accent)
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(div().opacity(0.5).child(quick.typed.clone()))
                    .child(rest)
            });
        Some(
            div()
                .id("terminal-quick-select")
                .debug_selector(|| "terminal-quick-select".to_owned())
                .role(gpui::Role::Status)
                .aria_label(SharedString::from(format!("{} labels", quick.labels.len())))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .font_family(family)
                .text_size(px(self.theme.typography.mono_size))
                .children(labels)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Labels are as short as the count allows and none starts another, so each is chosen the
    /// moment it is typed.
    #[test]
    fn labels_are_short_and_none_starts_another() {
        assert_eq!(labels(3, ALPHABET), ["a", "s", "d"]);
        assert_eq!(labels(0, ALPHABET), Vec::<String>::new());
        for count in [1, 26, 27, 51, 52, 200, 700] {
            let made = labels(count, ALPHABET);
            assert_eq!(made.len(), count);
            for (i, a) in made.iter().enumerate() {
                for (j, b) in made.iter().enumerate() {
                    assert!(i == j || !b.starts_with(a.as_str()), "{a} starts {b} ({count})");
                }
            }
            let longest = made.iter().map(String::len).max().unwrap_or(0);
            assert!(
                longest
                    <= if count <= 26 {
                        1
                    } else if count <= 676 {
                        2
                    } else {
                        3
                    }
            );
        }
        assert_eq!(labels(27, ALPHABET)[..2], ["s".to_owned(), "d".to_owned()]);
        assert_eq!(labels(2, "a"), ["a"], "one letter makes one label");
    }

    /// A label never runs past the grid's right edge: one that would is moved left to end at
    /// it, and one wider than the grid is not drawn.
    #[test]
    fn a_label_stays_inside_the_grid() {
        assert_eq!(label_col(10, 2, 80), Some(10));
        assert_eq!(label_col(79, 2, 80), Some(78));
        assert_eq!(label_col(79, 1, 80), Some(79));
        assert_eq!(label_col(0, 3, 2), None);
    }
}
