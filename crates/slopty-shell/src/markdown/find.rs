//! slopty: find in a thread, the paint half. The transcript names what it looks for and which
//! match is current ([`set`]); every painted text element of a row with matches washes its
//! matches under the glyphs, the current one stronger. Elements paint in document order, so
//! counting a row's matches as its elements paint finds the current one again, the way
//! [`super::selection`] rebuilds a drag across elements.
//!
//! The pure half (case-folded matching, a block's searchable text in paint order) is unit
//! tested; [`paint`] is called from the text elements' paint canvases in `render.rs`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use gpui::{BorderStyle, Hsla, IntoElement, Styled as _, Window, canvas, px, quad};

use super::parser::{Block, InlineRun};

/// What one transcript looks for, as its rows paint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindPaint {
    /// The words, as typed.
    pub needle: String,
    /// The rows holding a match: no other element paints any.
    pub rows: HashSet<String>,
    /// The current match: its row and its place among that row's matches.
    pub active: Option<(String, usize)>,
    /// The wash of every match, and of the current one.
    pub washes: (Hsla, Hsla),
}

thread_local! {
    /// Each surface's find, by surface id.
    static FIND: RefCell<HashMap<u64, FindPaint>> = RefCell::new(HashMap::new());
    /// Matches each row has painted this frame, by surface and row.
    static PAINTED: RefCell<HashMap<(u64, String), usize>> = RefCell::new(HashMap::new());
}

/// Look for `paint.needle` on `surface` from the next frame on.
pub fn set(surface: u64, paint: FindPaint) {
    FIND.with(|find| {
        find.borrow_mut().insert(surface, paint);
    });
}

/// Stop looking on `surface`.
pub fn clear(surface: u64) {
    FIND.with(|find| {
        find.borrow_mut().remove(&surface);
    });
}

/// A canvas painted before `surface`'s rows: their counts start again each frame.
pub fn frame_reset(surface: u64) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, _, _| {
            PAINTED.with(|painted| painted.borrow_mut().retain(|(s, _), _| *s != surface));
        },
    )
    .absolute()
    .size_0()
}

/// The row an element key belongs to: `{row}:{ix}` (text), `{row}:u` (a prompt) or
/// `{row}-code{n}-line{m}` (a code line).
pub fn row_of(key: &str) -> &str {
    if let Some(at) = key.rfind("-code")
        && key[at..].contains("-line")
    {
        return &key[..at];
    }
    key.rsplit_once(':').map_or(key, |(row, _)| row)
}

/// Every match of `needle` in `hay`, case folded, as byte ranges of `hay`, none overlapping.
pub fn matches(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = hay.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    'start: while i < chars.len() {
        let mut j = i;
        let mut k = 0;
        while k < needle.len() {
            let Some(&(_, c)) = chars.get(j) else {
                break 'start;
            };
            for lower in c.to_lowercase() {
                if needle.get(k) != Some(&lower) {
                    i += 1;
                    continue 'start;
                }
                k += 1;
            }
            j += 1;
        }
        let start = chars[i].0;
        let end = chars.get(j).map_or(hay.len(), |&(at, _)| at);
        out.push(start..end);
        i = j.max(i + 1);
    }
    out
}

fn runs_text(runs: &[InlineRun]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

/// The texts a block paints, one per text element, in paint order.
pub fn block_texts(block: &Block, out: &mut Vec<String>) {
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => out.push(runs_text(runs)),
        // slopty: terminal output paints its text, its escapes taken out.
        Block::CodeBlock { language, code } if super::ansi::is_ansi_fence(language.as_deref(), code) => {
            out.extend(super::ansi::parse(language.as_deref(), code).into_iter().map(|line| line.text));
        }
        Block::CodeBlock { code, .. } => out.extend(code.split('\n').map(str::to_owned)),
        Block::BlockQuote { children } => {
            for child in children {
                block_texts(child, out);
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for child in item {
                    block_texts(child, out);
                }
            }
        }
        Block::Table { header, rows, .. } => {
            out.extend(header.iter().map(|cell| runs_text(cell)));
            for row in rows {
                out.extend(row.iter().map(|cell| runs_text(cell)));
            }
        }
        Block::Rule => {}
        // A formula on its own is one text element, which copies as its source.
        Block::Math { tex } => out.push(format!("$${tex}$$")),
    }
}

/// How many matches of `needle` the texts hold together.
pub fn count(texts: &[String], needle: &str) -> usize {
    texts.iter().map(|text| matches(text, needle).len()).sum()
}

/// Wash the matches of element `key` (its full text `text`, laid out as `layout`; `displayed`
/// maps a range of `text` to the glyphs shown) under its glyphs, the current one stronger.
pub(crate) fn paint(
    window: &mut Window,
    surface: u64,
    key: &str,
    text: &str,
    layout: &gpui::TextLayout,
    displayed: &dyn Fn(Range<usize>) -> Range<usize>,
) {
    let Some(find) = FIND.with(|find| find.borrow().get(&surface).cloned()) else {
        return;
    };
    let row = row_of(key);
    if !find.rows.contains(row) {
        return;
    }
    let found = matches(text, &find.needle);
    if found.is_empty() {
        return;
    }
    let first = PAINTED.with(|painted| {
        let mut painted = painted.borrow_mut();
        let seen = painted.entry((surface, row.to_owned())).or_default();
        let first = *seen;
        *seen += found.len();
        first
    });
    for (nth, range) in found.into_iter().enumerate() {
        let active = find
            .active
            .as_ref()
            .is_some_and(|(at, place)| at == row && *place == first + nth);
        let wash = if active { find.washes.1 } else { find.washes.0 };
        for rect in super::render::range_rects(layout, &displayed(range), 0.0, 0.0) {
            window.paint_quad(quad(
                rect,
                px(2.0),
                wash,
                px(0.0),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }
}

/// The washes a match paints with: every match, and the current one. slopty: dark's are
/// stronger, as amber at a fifth reads as brown on near-black and the matches go missing (#16).
pub fn washes(theme: &crate::theme::Theme) -> (Hsla, Hsla) {
    let (every, current) = match theme.appearance {
        crate::theme::Appearance::Dark => (0.40, 0.75),
        _ => (0.22, 0.55),
    };
    (theme.warning.opacity(every), theme.warning.opacity(current))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_fold_case_and_never_overlap() {
        assert_eq!(matches("Find the find, FIND!", "find"), [0..4, 9..13, 15..19]);
        assert_eq!(matches("aaaa", "aa"), [0..2, 2..4]);
        let sharp = matches("Straße STRASSE", "straße");
        assert_eq!((sharp.len(), sharp.first().cloned()), (1, Some(0..7)));
        assert!(matches("anything", "").is_empty());
        assert!(matches("short", "longer than it").is_empty());
        assert_eq!(matches("éÉ", "é"), [0..2, 2..4]);
    }

    /// slopty: dark's matches wash stronger than light's, every one and the current (#16).
    #[test]
    fn dark_matches_stay_visible() {
        let (dark, light) = (crate::theme::Theme::dark(), crate::theme::Theme::light());
        assert_eq!(washes(&dark), (dark.warning.opacity(0.40), dark.warning.opacity(0.75)));
        assert_eq!(washes(&light), (light.warning.opacity(0.22), light.warning.opacity(0.55)));
    }

    #[test]
    fn a_key_names_its_row() {
        assert_eq!(row_of("a1#t0.2:3"), "a1#t0.2");
        assert_eq!(row_of("plan-card:t#plan:t.4:0"), "plan-card:t#plan:t.4");
        assert_eq!(row_of("u1:u"), "u1");
        assert_eq!(row_of("a1#t0.1-code0-line7"), "a1#t0.1");
    }

    #[test]
    fn a_block_s_texts_follow_paint_order() {
        let tree = crate::markdown::parser::parse_full(
            "# Head\n\nSome *find* text.\n\n- one find\n- two\n\n```\nfind me\nnot\n```\n\n| a | find |\n|---|---|\n| find | b |\n",
        );
        let mut texts = Vec::new();
        for top in &tree.blocks {
            block_texts(&top.block, &mut texts);
        }
        assert_eq!(texts[0], "Head");
        assert_eq!(texts[1], "Some find text.");
        assert_eq!(count(&texts, "find"), 5);
    }

    /// slopty: terminal output's texts are what it paints, its escapes taken out, spelled or not:
    /// what find matches and what a screen reader reads.
    #[test]
    fn terminal_output_reads_without_its_escapes() {
        let tree = crate::markdown::parser::parse_full(
            "```ansi\n\\x1b[1;32m✓\\x1b[0m 12 passed\n\\x1b[31merror\\x1b[0m: x\n```\n\n```\n\u{1b}[2mdim\u{1b}[0m\n```\n",
        );
        let mut texts = Vec::new();
        for top in &tree.blocks {
            block_texts(&top.block, &mut texts);
        }
        texts.retain(|text| !text.is_empty());
        assert_eq!(texts, ["✓ 12 passed", "error: x", "dim"]);
    }
}
