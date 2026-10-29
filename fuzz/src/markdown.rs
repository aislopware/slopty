//! An agent's markdown, streamed through the parser every transcript row is drawn from
//! (`slopty_markdown`).
//!
//! The text is whatever the model wrote, arriving a delta at a time as it streams. The first
//! byte is the delta's size in bytes (1 to 255, widened to the next character boundary); the
//! rest is the text, cut at [`MAX`] bytes.
//!
//! What must hold, beside no panic (but one inside pulldown-cmark, which the parser catches and
//! shows the source as text for: [`CONTAINED`]):
//! - **parity:** after each delta the streamed tree is the one-shot parse of what arrived so far,
//!   and at the end the display tree is the one a single `set_text` of the whole shows;
//! - **stability:** the blocks an append says it left alone are the blocks it had before;
//! - **ranges:** every block's range is a run of the source between character boundaries, and the
//!   blocks come in source order.

use slopty_markdown::{BlockTree, IncrementalParser, parse_full};

/// Where the panics `slopty_markdown` catches are raised: pulldown-cmark's own sources.
pub const CONTAINED: &[&str] = &["/pulldown-cmark-0."];

/// The longest text taken: a parse per delta costs the whole text, so a long one is a slow run
/// that finds nothing a shorter one does not.
pub const MAX: usize = 8 * 1024;

/// The most deltas after which the streamed tree is held to a one-shot parse; past them, the
/// check runs at as many evenly spread deltas and at the last.
const CHECKS: usize = 48;

/// Run one input.
pub fn run(data: &[u8]) {
    let Some((&delta, rest)) = data.split_first() else { return };
    let source = String::from_utf8_lossy(rest.get(..MAX).unwrap_or(rest));
    let delta = usize::from(delta).max(1);
    let deltas = source.len().div_ceil(delta).max(1);
    let every = deltas.div_ceil(CHECKS).max(1);
    let mut parser = IncrementalParser::new();
    let mut before = BlockTree::default();
    let (mut start, mut step) = (0_usize, 0_usize);
    while start < source.len() {
        let mut end = start.saturating_add(delta).min(source.len());
        while !source.is_char_boundary(end) {
            end = end.saturating_add(1);
        }
        parser.append(source.get(start..end).expect("a delta between character boundaries"));
        start = end;
        step = step.saturating_add(1);
        let tree = parser.tree();
        in_source_order(tree, parser.source());
        let kept = parser.stable_prefix_blocks().min(tree.blocks.len());
        assert_eq!(
            tree.blocks.get(..kept),
            before.blocks.get(..kept),
            "an append changed a block it said it left alone"
        );
        if step.is_multiple_of(every) || start == source.len() {
            assert_eq!(tree, &parse_full(parser.source()), "the stream left a one-shot parse");
        }
        let _shown = parser.display_tree();
        before = tree.clone();
    }
    let mut settled = IncrementalParser::new();
    settled.set_text(&source);
    assert_eq!(parser.display_tree(), settled.display_tree(), "the stream shows another tree");
}

/// Every block's range is in `source`, between character boundaries, none before the last.
fn in_source_order(tree: &BlockTree, source: &str) {
    let mut last = 0_usize;
    for block in &tree.blocks {
        let range = &block.range;
        assert!(range.start <= range.end && range.end <= source.len(), "{range:?} in {source:?}");
        assert!(
            source.is_char_boundary(range.start) && source.is_char_boundary(range.end),
            "{range:?} splits a character of {source:?}"
        );
        assert!(range.start >= last, "a block at {range:?} before one at {last}");
        last = range.start;
    }
}
