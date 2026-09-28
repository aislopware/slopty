//! Allocation budgets on the client's side of a diff, counted by `slopty_testkit::alloc` on the
//! test's own thread: a row update applied to the screen and the scrollback it shares its lines
//! with. The counts are exact and do not depend on the machine's load, so they gate.
//! `docs/decisions/testing.md`, "Allocation budgets".

#[cfg(test)]
#[expect(clippy::arithmetic_side_effects, reason = "line indices and counts of a few thousand")]
mod allocs {
    use std::sync::Arc;

    use slopty_grid::{Line, LineIndex, RowUpdate, Screen, Scrollback, Style};
    use slopty_testkit::alloc::{self, Counting};

    #[global_allocator]
    static ALLOC: Counting = Counting;

    const COLS: u16 = 200;
    const ROWS: u16 = 60;
    const HISTORY: usize = 10_000;

    fn line(i: u64) -> Line {
        Line::from_text(&format!("line {i} the quick brown fox"), COLS, Style::default())
    }

    /// A screen and a scrollback at their steady size: the history full, the screen's rows its
    /// last lines.
    fn full() -> (Screen, Scrollback, u64) {
        let mut screen = Screen::new(COLS, ROWS);
        let mut history = Scrollback::new(HISTORY);
        let total = u64::try_from(HISTORY).unwrap() * 2;
        for i in 0..total {
            history.insert(LineIndex(i), line(i));
        }
        let top = total - u64::from(ROWS);
        history.set_extent(LineIndex(total - u64::try_from(HISTORY).unwrap()), total);
        history.set_view(LineIndex(top), LineIndex(top));
        for row in 0..ROWS {
            let shared = history.shared(LineIndex(top + u64::from(row))).unwrap();
            screen.apply_shared(row, shared).unwrap();
        }
        (screen, history, top)
    }

    /// An echo's row replaces the one on screen and in the scrollback: one block, the shared
    /// line, whatever the size of either.
    #[test]
    fn applying_an_echo_row_is_one_block() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let (mut screen, mut history, top) = full();
        let row = ROWS - 1;
        let update = RowUpdate { row, line: line(7) };
        let ((), used) = alloc::measure(|| {
            let shared = Arc::new(update.line);
            screen.apply_shared(update.row, Arc::clone(&shared)).unwrap();
            history.insert_shared(LineIndex(top + u64::from(row)), shared);
        });
        eprintln!("an echo row applied: {used}");
        assert_eq!(used.blocks, 1, "{used}");
        let plain = RowUpdate { row, line: line(8) };
        let ((), used) = alloc::measure(|| screen.apply(plain).unwrap());
        assert_eq!(used.blocks, 1, "Screen::apply: {used}");
    }

    /// Lines scrolling into a full history cost their shared line and, now and then, a node of
    /// the index: 1.2 blocks a line at most. Until 2026-09-29 each move of the oldest line
    /// rebuilt the index along the split (`split_off`), six blocks and 1.4 KiB a line.
    #[test]
    fn lines_scrolling_into_a_full_history_stay_bounded() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let (_screen, mut history, top) = full();
        let first = top + u64::from(ROWS);
        let count = 1_000_u64;
        let incoming: Vec<Line> = (first..first + count).map(line).collect();
        let ((), used) = alloc::measure(|| {
            for (i, line) in (first..).zip(incoming) {
                let total = i + 1;
                history.set_extent(LineIndex(total - u64::try_from(HISTORY).unwrap()), total);
                history.set_view(
                    LineIndex(total - u64::from(ROWS)),
                    LineIndex(total - u64::from(ROWS)),
                );
                history.insert(LineIndex(i), line);
            }
        });
        eprintln!("{count} lines scrolled into a full history: {used}");
        assert!(used.blocks <= count * 6 / 5, "{used} for {count} lines");
        assert!(used.bytes <= count * 128, "{used} for {count} lines");
    }
}
