//! Words from the file offered as the caret's word is typed, as plain functions.
//!
//! No language server: the candidates are the other words of the same text that start with
//! what is typed, the nearest to the caret first, as Sublime's and Zed's buffer words are.
//! [`Words`] hands them to gpui-kit's completion menu.

use std::ops::Range;

use gpui::{App, AppContext as _, Task, Window};
use gpui_kit::base::input::lsp_types::{
    self, CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse,
    CompletionTextEdit, TextEdit,
};
use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use rustc_hash::FxHashMap;

/// Candidates offered at most.
pub const WORDS_MAX: usize = 50;
/// A typed word shorter than this offers nothing, so a single letter does not open the list.
pub const PREFIX_MIN: usize = 2;
/// How far back the typed word is looked for; a longer run is not a word anyone completes.
const PREFIX_BYTES: usize = 128;
/// The text round the caret the words are read from, so a keystroke in a 16 MiB file costs what
/// one in a 1 MiB file does.
const SCAN_BYTES: usize = 1 << 20;

/// Whether `c` is part of a word: a letter, a digit or `_`.
#[must_use]
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word that ends at `caret` in `before`, the text up to the caret: its start, in bytes.
#[must_use]
pub fn prefix_start(before: &str, caret: usize) -> usize {
    let floor = caret.saturating_sub(PREFIX_BYTES);
    before
        .get(..caret)
        .unwrap_or_default()
        .char_indices()
        .rev()
        .take_while(|&(at, c)| at >= floor && is_word(c))
        .last()
        .map_or(caret, |(at, _)| at)
}

/// The words of `text` that start with `prefix` and are longer, each once, the nearest to
/// `typed` (the range of the word being typed, itself left out) first.
///
/// A prefix with no capital matches in any case, one with a capital as typed: the find bar's
/// smart case.
#[must_use]
pub fn candidates(text: &str, prefix: &str, typed: Range<usize>) -> Vec<String> {
    if prefix.chars().count() < PREFIX_MIN {
        return Vec::new();
    }
    let any_case = !prefix.chars().any(char::is_uppercase);
    let folded = prefix.to_lowercase();
    let starts = |word: &str| {
        word.get(..prefix.len()).is_some_and(|head| {
            if !any_case {
                head == prefix
            } else if head.is_ascii() {
                head.eq_ignore_ascii_case(prefix)
            } else {
                head.to_lowercase() == folded
            }
        })
    };
    let mut nearest: FxHashMap<&str, usize> = FxHashMap::default();
    for (at, word) in words(text) {
        if at == typed.start || word.len() <= prefix.len() || !starts(word) {
            continue;
        }
        let distance =
            if at < typed.start { typed.start.abs_diff(at) } else { at.abs_diff(typed.end) };
        nearest.entry(word).and_modify(|d| *d = (*d).min(distance)).or_insert(distance);
    }
    let mut found: Vec<(usize, &str)> = nearest.into_iter().map(|(w, d)| (d, w)).collect();
    found.sort_unstable();
    found.into_iter().take(WORDS_MAX).map(|(_, w)| w.to_owned()).collect()
}

/// The file's words for gpui-kit's completion menu, read off the UI thread on each word
/// character typed.
#[derive(Clone, Copy, Debug)]
pub struct Words;

impl CompletionProvider for Words {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let text = text.clone();
        cx.background_spawn(async move { Ok(CompletionResponse::Array(offered(&text, offset))) })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        let mut chars = new_text.chars();
        chars.next().is_some_and(is_word) && chars.next().is_none()
    }
}

/// The menu's items at `caret`: each candidate replacing the typed word.
fn offered(text: &Rope, caret: usize) -> Vec<CompletionItem> {
    let caret = caret.min(text.len());
    let half = SCAN_BYTES / 2;
    let lo = text.line_start_offset(text.offset_to_point(caret.saturating_sub(half)).row);
    let hi = text.line_end_offset(text.offset_to_point(caret.saturating_add(half)).row);
    let window = text.slice(lo..hi.max(caret)).to_string();
    let at = caret.saturating_sub(lo);
    let start = prefix_start(&window, at);
    let prefix = window.get(start..at).unwrap_or_default();
    let range = lsp_types::Range::new(
        text.offset_to_position(lo.saturating_add(start)),
        text.offset_to_position(caret),
    );
    candidates(&window, prefix, start..at)
        .into_iter()
        .map(|word| CompletionItem {
            label: word.clone(),
            kind: Some(CompletionItemKind::TEXT),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text: word })),
            ..CompletionItem::default()
        })
        .collect()
}

/// Every word of `text` with its start, in bytes.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut start = None;
    text.char_indices().chain(std::iter::once((text.len(), ' '))).filter_map(move |(at, c)| match (
        is_word(c),
        start,
    ) {
        (true, None) => {
            start = Some(at);
            None
        }
        (false, Some(from)) => {
            start = None;
            text.get(from..at).map(|word| (from, word))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(text: &str) -> Vec<String> {
        let caret = text.find('|').unwrap_or(text.len());
        let text = text.replacen('|', "", 1);
        let start = prefix_start(&text, caret);
        candidates(&text, text.get(start..caret).unwrap_or_default(), start..caret)
    }

    #[test]
    fn the_typed_word_is_found_back_to_its_start() {
        assert_eq!(prefix_start("let value_2", 11), 4);
        assert_eq!(prefix_start("a.b", 3), 2);
        assert_eq!(prefix_start("x ", 2), 2, "after a space there is no word");
        assert_eq!(prefix_start("é_ok", "é_ok".len()), 0, "letters past ASCII are word letters");
    }

    #[test]
    fn the_nearest_words_with_the_prefix_come_first_each_once() {
        let text = "compute_far\nlet c = compute_near(1) + compute_near(2);\nco|\ncomputed";
        assert_eq!(offered(text), ["computed", "compute_near", "compute_far"]);
    }

    #[test]
    fn the_typed_word_and_shorter_words_are_not_offered() {
        assert!(offered("al| al").is_empty(), "an equal word adds nothing");
        assert_eq!(offered("alpha alp|"), ["alpha"], "the word being typed is left out");
        assert!(offered("alpha a|").is_empty(), "one letter offers nothing");
    }

    #[test]
    fn a_prefix_with_a_capital_matches_as_typed() {
        assert_eq!(offered("Value value_of Va|"), ["Value"]);
        assert_eq!(offered("Value value_of va|"), ["value_of", "Value"]);
    }

    #[test]
    fn at_most_the_cap_is_offered() {
        let text = (0..WORDS_MAX * 2).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" ");
        assert_eq!(offered(&format!("{text} wo|")).len(), WORDS_MAX);
    }
}
