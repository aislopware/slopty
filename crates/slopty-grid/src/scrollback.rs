//! Client-side scrollback cache keyed by absolute line index.
//!
//! The worker numbers every line the terminal has ever produced (0 = first line at session start).
//! The visible screen is the last `rows` lines when the viewport is at the bottom. A client keeps
//! whatever lines it has received in a sparse window and asks for the ranges it is missing when
//! the user scrolls, so scrolling is local and instant once a range is cached.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Bound, Range};
use std::sync::Arc;

use crate::Line;

/// Absolute line number since session start. Monotonic; never reused.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Debug,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct LineIndex(pub u64);

impl LineIndex {
    /// The next index.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// `self + n`.
    #[must_use]
    pub const fn offset(self, n: u64) -> Self {
        Self(self.0.saturating_add(n))
    }
}

/// Cache statistics for the debug overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScrollbackStats {
    /// Lines currently cached.
    pub cached: usize,
    /// Cache capacity.
    pub capacity: usize,
    /// Total lines the worker reports (history + screen).
    pub total: u64,
}

/// No lines: what a trim that spares nothing spares.
const NOTHING: Range<LineIndex> = LineIndex(0)..LineIndex(0);

/// Sparse, bounded cache of lines by absolute index.
#[derive(Clone, Debug)]
pub struct Scrollback {
    /// Shared with [`crate::Screen`]: a row that scrolls off the screen into history is one
    /// allocation held twice, not a copy.
    lines: BTreeMap<LineIndex, Arc<Line>>,
    /// The indices of the cached lines that start a prompt, so a block's prompt is a range
    /// query and not a walk over the cache (a sticky header asks on every frame of every
    /// shell; MEASUREMENTS 2026-09-13, the 20-shell zoom).
    prompts: BTreeSet<LineIndex>,
    capacity: usize,
    /// Number of lines the worker currently has (history + visible rows).
    total: u64,
    /// The first index the worker can still serve (older lines were evicted worker-side).
    oldest: LineIndex,
    /// The line the viewer looks at: past capacity, the lines farthest from it go first.
    focus: LineIndex,
    /// The screen's first row: it and every row after it are never evicted, so a screen that
    /// moves down finds its rows here.
    screen: LineIndex,
}

impl Scrollback {
    /// A cache holding at most `capacity` lines.
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self {
            lines: BTreeMap::new(),
            prompts: BTreeSet::new(),
            capacity,
            total: 0,
            oldest: LineIndex(0),
            // Until told otherwise the newest lines are looked at: the oldest go first.
            focus: LineIndex(u64::MAX),
            screen: LineIndex(u64::MAX),
        }
    }

    /// Where the viewer is: `top` is the first line it shows, `screen` the screen's first row.
    /// Eviction keeps what is near `top` and never takes a screen row.
    pub const fn set_view(&mut self, top: LineIndex, screen: LineIndex) {
        self.focus = top;
        self.screen = screen;
    }

    /// Record the worker's current line count and oldest retained index.
    pub fn set_extent(&mut self, oldest: LineIndex, total: u64) {
        self.total = total;
        // Only when the worker actually dropped something: this runs on every frame, and
        // `split_off` walks and rebuilds the map whether or not anything is below `oldest`.
        if oldest > self.oldest {
            self.oldest = oldest;
            // Lines the worker dropped are useless; drop them here too.
            self.lines = self.lines.split_off(&oldest);
            self.prompts = self.prompts.split_off(&oldest);
        } else {
            self.oldest = oldest;
        }
        self.trim(&NOTHING);
    }

    /// Total lines on the worker.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// Oldest index still retrievable.
    #[must_use]
    pub const fn oldest(&self) -> LineIndex {
        self.oldest
    }

    /// Insert or replace a line.
    pub fn insert(&mut self, index: LineIndex, line: Line) {
        self.insert_shared(index, Arc::new(line));
    }

    /// Insert or replace a line the caller already shares (with the screen, in practice).
    pub fn insert_shared(&mut self, index: LineIndex, line: Arc<Line>) {
        self.put(index, line);
        self.trim(&NOTHING);
    }

    /// Insert a contiguous batch starting at `start`. None of it is evicted to make room for
    /// the rest: a fetch far from the view is still there when the batch is in.
    pub fn insert_batch(&mut self, start: LineIndex, lines: impl IntoIterator<Item = Line>) {
        let mut idx = start;
        for line in lines {
            self.put(idx, Arc::new(line));
            idx = idx.next();
        }
        self.trim(&(start..idx));
    }

    fn put(&mut self, index: LineIndex, line: Arc<Line>) {
        if index < self.oldest {
            return;
        }
        if line.mark.starts_prompt() {
            self.prompts.insert(index);
        } else {
            self.prompts.remove(&index);
        }
        self.lines.insert(index, line);
        self.total = self.total.max(index.0.saturating_add(1));
    }

    /// The start of the nearest cached prompt strictly above `index`.
    #[must_use]
    pub fn prompt_before(&self, index: LineIndex) -> Option<LineIndex> {
        self.prompts.range(..index).next_back().copied()
    }

    /// The start of the nearest cached prompt strictly below `index`.
    #[must_use]
    pub fn prompt_after(&self, index: LineIndex) -> Option<LineIndex> {
        self.prompts.range((Bound::Excluded(index), Bound::Unbounded)).next().copied()
    }

    /// A cached line.
    #[must_use]
    pub fn get(&self, index: LineIndex) -> Option<&Line> {
        self.lines.get(&index).map(AsRef::as_ref)
    }

    /// A cached line, shared: what the screen re-adopts when the viewport moves.
    #[must_use]
    pub fn shared(&self, index: LineIndex) -> Option<Arc<Line>> {
        self.lines.get(&index).map(Arc::clone)
    }

    /// Sub-ranges of `[start, start + count)` that are not cached and are still retrievable, so
    /// the client can request exactly those from the worker.
    #[must_use]
    pub fn missing(&self, start: LineIndex, count: u64) -> Vec<(LineIndex, u64)> {
        let mut gaps = Vec::new();
        let end = start.offset(count).0.min(self.total);
        let mut cursor = start.0.max(self.oldest.0);
        let mut gap_start: Option<u64> = None;
        while cursor < end {
            let cached = self.lines.contains_key(&LineIndex(cursor));
            match (cached, gap_start) {
                (false, None) => gap_start = Some(cursor),
                (true, Some(gs)) => {
                    gaps.push((LineIndex(gs), cursor.saturating_sub(gs)));
                    gap_start = None;
                }
                _ => {}
            }
            cursor = cursor.saturating_add(1);
        }
        if let Some(gs) = gap_start {
            gaps.push((LineIndex(gs), end.saturating_sub(gs)));
        }
        gaps
    }

    /// Statistics.
    #[must_use]
    pub fn stats(&self) -> ScrollbackStats {
        ScrollbackStats { cached: self.lines.len(), capacity: self.capacity, total: self.total }
    }

    /// Evict past capacity, farthest from the view first, sparing `keep` and the screen.
    /// Following the output that is the oldest line; scrolled up, it is whichever end of the
    /// cache is farther from the top of the view, so lines fetched there stay.
    fn trim(&mut self, keep: &Range<LineIndex>) {
        while self.lines.len() > self.capacity {
            let first = self.lines.keys().next().copied();
            let low = match first {
                Some(index) if keep.contains(&index) => {
                    self.lines.range(keep.end..).next().map(|(index, _)| *index)
                }
                other => other,
            }
            .filter(|index| *index < self.screen);
            let last = self.lines.range(..self.screen).next_back().map(|(index, _)| *index);
            let high = match last {
                Some(index) if keep.contains(&index) => {
                    self.lines.range(..keep.start).next_back().map(|(index, _)| *index)
                }
                other => other,
            };
            let distance = |index: LineIndex| index.0.abs_diff(self.focus.0);
            let victim = match (low, high) {
                (Some(low), Some(high)) => {
                    if distance(high) > distance(low) {
                        high
                    } else {
                        low
                    }
                }
                (Some(only), None) | (None, Some(only)) => only,
                (None, None) => break,
            };
            self.lines.remove(&victim);
            self.prompts.remove(&victim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Style;

    fn l(t: &str) -> Line {
        Line::from_text(t, 8, Style::DEFAULT)
    }

    #[test]
    fn missing_ranges_are_exact() {
        let mut sb = Scrollback::new(100);
        sb.set_extent(LineIndex(0), 10);
        sb.insert(LineIndex(2), l("2"));
        sb.insert(LineIndex(3), l("3"));
        sb.insert(LineIndex(7), l("7"));
        assert_eq!(
            sb.missing(LineIndex(0), 10),
            vec![(LineIndex(0), 2), (LineIndex(4), 3), (LineIndex(8), 2)]
        );
        assert_eq!(sb.missing(LineIndex(2), 2), vec![], "fully cached");
        assert_eq!(sb.missing(LineIndex(8), 50), vec![(LineIndex(8), 2)], "clamped to total");
    }

    /// The worker's extent arrives on every frame; only a real advance may throw lines away.
    #[test]
    fn a_repeated_extent_keeps_the_cache() {
        let mut sb = Scrollback::new(100);
        sb.set_extent(LineIndex(5), 50);
        sb.insert(LineIndex(6), l("6"));
        sb.insert(LineIndex(7), l("7"));
        for _ in 0..10 {
            sb.set_extent(LineIndex(5), 60);
        }
        assert_eq!(sb.stats().cached, 2, "nothing was dropped by an unchanged oldest");
        assert_eq!(sb.total(), 60);
        sb.set_extent(LineIndex(7), 60);
        assert!(sb.get(LineIndex(6)).is_none(), "an advance does drop what the worker dropped");
        assert!(sb.get(LineIndex(7)).is_some());
    }

    /// The prompt index follows every insert, replacement and eviction, so a block's prompt
    /// is found without a walk.
    #[test]
    fn prompts_are_indexed_through_replacement_and_eviction() {
        let mut sb = Scrollback::new(4);
        sb.set_extent(LineIndex(0), 10);
        let prompt = |t: &str| {
            let mut line = l(t);
            line.mark = crate::SemanticMark::Prompt { exit: None, input: Some(2) };
            line
        };
        sb.insert(LineIndex(1), prompt("$ a"));
        sb.insert(LineIndex(2), l("out"));
        sb.insert(LineIndex(3), prompt("$ b"));
        assert_eq!(sb.prompt_before(LineIndex(3)), Some(LineIndex(1)));
        assert_eq!(sb.prompt_before(LineIndex(1)), None, "strictly above");
        assert_eq!(sb.prompt_after(LineIndex(1)), Some(LineIndex(3)));
        assert_eq!(sb.prompt_after(LineIndex(3)), None, "strictly below");
        sb.insert(LineIndex(1), l("erased in place"));
        assert_eq!(sb.prompt_before(LineIndex(3)), None, "a replaced row leaves the index");
        sb.insert(LineIndex(1), prompt("$ a"));
        sb.insert(LineIndex(4), l("4"));
        sb.insert(LineIndex(5), l("5"));
        assert!(sb.get(LineIndex(1)).is_none(), "evicted by the capacity");
        assert_eq!(sb.prompt_before(LineIndex(5)), Some(LineIndex(3)));
        sb.set_extent(LineIndex(4), 10);
        assert_eq!(sb.prompt_before(LineIndex(5)), None, "the worker's drop clears it too");
    }

    /// A line the screen also holds is one allocation, not two.
    #[test]
    fn a_shared_line_is_not_copied() {
        let mut sb = Scrollback::new(10);
        sb.set_extent(LineIndex(0), 10);
        let line = Arc::new(l("shared"));
        sb.insert_shared(LineIndex(3), Arc::clone(&line));
        let back = sb.shared(LineIndex(3)).expect("cached");
        assert!(Arc::ptr_eq(&line, &back), "the same allocation came back");
        assert_eq!(Arc::strong_count(&line), 3, "the caller, the cache and `back`");
        assert_eq!(sb.get(LineIndex(3)).map(Line::text).as_deref(), Some("shared"));
    }

    #[test]
    fn a_batch_lands_at_consecutive_indices() {
        let mut sb = Scrollback::new(10);
        sb.set_extent(LineIndex(0), 3);
        sb.insert_batch(LineIndex(0), [l("a"), l("b"), l("c")]);
        assert_eq!(sb.missing(LineIndex(0), 3), vec![]);
        assert_eq!(sb.get(LineIndex(2)).map(Line::text).as_deref(), Some("c"));
    }

    /// Scrolled up, what is near the view stays and the far end goes; a batch fetched
    /// anywhere survives its own insert; the screen's rows are never taken.
    #[test]
    fn eviction_is_farthest_from_the_view_and_spares_the_batch_and_the_screen() {
        let mut sb = Scrollback::new(4);
        sb.set_extent(LineIndex(0), 100);
        sb.set_view(LineIndex(10), LineIndex(96));
        for i in [8_u64, 9, 10, 11, 96, 97] {
            sb.insert(LineIndex(i), l(&i.to_string()));
        }
        let held = |sb: &Scrollback| {
            (0..100).filter(|&i| sb.get(LineIndex(i)).is_some()).collect::<Vec<u64>>()
        };
        assert_eq!(held(&sb), [10, 11, 96, 97], "the view's lines went last, the screen stays");
        sb.insert_batch(LineIndex(50), [l("50"), l("51"), l("52")]);
        assert_eq!(held(&sb), [50, 51, 52, 96, 97], "the batch is kept whole, over capacity");
        sb.insert(LineIndex(12), l("12"));
        assert_eq!(held(&sb), [12, 50, 96, 97], "then the farthest from the view goes");
        sb.set_view(LineIndex(96), LineIndex(96));
        sb.insert(LineIndex(98), l("98"));
        assert_eq!(held(&sb), [50, 96, 97, 98], "following again: the oldest goes");
    }

    #[test]
    fn eviction_is_oldest_first_and_worker_extent_wins() {
        let mut sb = Scrollback::new(3);
        sb.set_extent(LineIndex(0), 100);
        for i in 0..5_u64 {
            sb.insert(LineIndex(i), l(&i.to_string()));
        }
        assert_eq!(sb.stats().cached, 3);
        assert!(sb.get(LineIndex(1)).is_none());
        assert!(sb.get(LineIndex(4)).is_some());
        sb.set_extent(LineIndex(4), 100);
        assert!(sb.get(LineIndex(3)).is_none(), "worker evicted it, so do we");
        sb.insert(LineIndex(2), l("late"));
        assert!(sb.get(LineIndex(2)).is_none(), "below the worker's oldest is ignored");
        assert_eq!(sb.missing(LineIndex(0), 6), vec![(LineIndex(5), 1)], "starts at oldest");
    }
}
