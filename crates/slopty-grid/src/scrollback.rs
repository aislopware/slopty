//! Client-side scrollback cache keyed by absolute line index.
//!
//! The host numbers every line the terminal has ever produced (0 = first line at session start).
//! The visible screen is the last `rows` lines when the viewport is at the bottom. A client keeps
//! whatever lines it has received in a sparse window and asks for the ranges it is missing when
//! the user scrolls, so scrolling is local and instant once a range is cached.

use std::collections::BTreeMap;

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
    lines: BTreeMap<LineIndex, Line>,
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
        Self { lines: BTreeMap::new(), capacity, total: 0, oldest: LineIndex(0) }
    }

    /// Record the host's current line count and oldest retained index.
    pub fn set_extent(&mut self, oldest: LineIndex, total: u64) {
        self.oldest = oldest;
        self.total = total;
        // Lines the host dropped are useless; drop them here too.
        self.lines = self.lines.split_off(&oldest);
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
        if index < self.oldest {
            return;
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

    /// A cached line.
    #[must_use]
    pub fn get(&self, index: LineIndex) -> Option<&Line> {
        self.lines.get(&index)
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
            if self.lines.pop_first().is_none() {
                break;
            }
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
