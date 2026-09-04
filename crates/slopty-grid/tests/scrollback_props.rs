//! `Scrollback::missing` must return exactly the retrievable, uncached indices, as disjoint
//! ascending ranges, regardless of insert order and eviction.

#[cfg(test)]
mod props {
    use proptest::prelude::*;
    use slopty_grid::{Line, LineIndex, Scrollback, Style};

    proptest! {
        #[test]
        fn missing_is_exact(
            capacity in 1_usize..64,
            total in 0_u64..200,
            oldest in 0_u64..50,
            inserts in proptest::collection::vec(0_u64..200, 0..100),
            start in 0_u64..220,
            count in 0_u64..80,
        ) {
            let oldest = oldest.min(total);
            let mut sb = Scrollback::new(capacity);
            sb.set_extent(LineIndex(oldest), total);
            for i in inserts {
                sb.insert(LineIndex(i), Line::from_text("x", 1, Style::DEFAULT));
            }
            let gaps = sb.missing(LineIndex(start), count);

            // Reconstruct the set of missing indices from the ranges.
            let mut from_gaps = Vec::new();
            let mut last_end = None;
            for (s, n) in &gaps {
                prop_assert!(*n > 0, "empty range");
                if let Some(e) = last_end {
                    prop_assert!(s.0 > e, "ranges must be ascending and disjoint");
                }
                from_gaps.extend(s.0..s.0.saturating_add(*n));
                last_end = Some(s.0.saturating_add(*n).saturating_sub(1));
            }
            let expected: Vec<u64> = (start..start.saturating_add(count))
                .filter(|i| *i >= oldest && *i < sb.total() && sb.get(LineIndex(*i)).is_none())
                .collect();
            prop_assert_eq!(from_gaps, expected);
            prop_assert!(sb.stats().cached <= capacity);
        }
    }
}
