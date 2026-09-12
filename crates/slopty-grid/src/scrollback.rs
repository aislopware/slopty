//! Client-side scrollback cache keyed by absolute line index.
//!
//! The host numbers every line the terminal has ever produced (0 = first line at session start).
//! The visible screen is the last `rows` lines when the viewport is at the bottom. A client keeps
//! whatever lines it has received in a sparse window and asks for the ranges it is missing when
//! the user scrolls, so scrolling is local and instant once a range is cached.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
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
    /// Total lines the host reports (history + screen).
    pub total: u64,
}

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
    /// Number of lines the host currently has (history + visible rows).
    total: u64,
    /// The first index the host can still serve (older lines were evicted host-side).
    oldest: LineIndex,
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
        }
    }

    /// Record the host's current line count and oldest retained index.
    pub fn set_extent(&mut self, oldest: LineIndex, total: u64) {
        self.total = total;
        // Only when the host actually dropped something: this runs on every frame, and
        // `split_off` walks and rebuilds the map whether or not anything is below `oldest`.
        if oldest > self.oldest {
            self.oldest = oldest;
            // Lines the host dropped are useless; drop them here too.
            self.lines = self.lines.split_off(&oldest);
            self.prompts = self.prompts.split_off(&oldest);
        } else {
            self.oldest = oldest;
        }
        self.trim();
    }

    /// Total lines on the host.
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
        self.trim();
    }

    /// Insert a contiguous batch starting at `start`.
    pub fn insert_batch(&mut self, start: LineIndex, lines: impl IntoIterator<Item = Line>) {
        let mut idx = start;
        for line in lines {
            self.insert(idx, line);
            idx = idx.next();
        }
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
    /// the client can request exactly those from the host.
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

    /// Evict the oldest lines past capacity. Newest lines are the ones a user is most likely to
    /// scroll to, so eviction is strictly oldest-first.
    fn trim(&mut self) {
        while self.lines.len() > self.capacity {
            let Some((index, _)) = self.lines.pop_first() else { break };
            self.prompts.remove(&index);
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

    /// The host's extent arrives on every frame; only a real advance may throw lines away.
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
        assert!(sb.get(LineIndex(6)).is_none(), "an advance does drop what the host dropped");
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
        assert_eq!(sb.prompt_before(LineIndex(5)), None, "the host's drop clears it too");
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

    #[test]
    fn eviction_is_oldest_first_and_host_extent_wins() {
        let mut sb = Scrollback::new(3);
        sb.set_extent(LineIndex(0), 100);
        for i in 0..5_u64 {
            sb.insert(LineIndex(i), l(&i.to_string()));
        }
        assert_eq!(sb.stats().cached, 3);
        assert!(sb.get(LineIndex(1)).is_none());
        assert!(sb.get(LineIndex(4)).is_some());
        sb.set_extent(LineIndex(4), 100);
        assert!(sb.get(LineIndex(3)).is_none(), "host evicted it, so do we");
        sb.insert(LineIndex(2), l("late"));
        assert!(sb.get(LineIndex(2)).is_none(), "below the host's oldest is ignored");
        assert_eq!(sb.missing(LineIndex(0), 6), vec![(LineIndex(5), 1)], "starts at oldest");
    }
}
