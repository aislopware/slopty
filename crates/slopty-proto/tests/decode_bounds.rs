//! What a peer's bytes may decode into. A line leaves its trailing blanks off the wire, so a
//! blank line of any width is a few bytes: before the bounds, 703 bytes of `TermEvent::Lines`
//! decoded into 314 MB, and one 16 MB frame of them asked for terabytes.
//! `docs/decisions/terminal.md`, "Decoded lines are bounded".

#[cfg(test)]
mod bounds {
    use slopty_grid::{
        Cell, Cursor, Line, LineIndex, MAX_COLS, MAX_ROWS, TermModes, with_cell_budget,
    };
    use slopty_proto::codec::{self, MAX_DECODED_CELLS, MAX_FRAME_BYTES};
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::{Frame, MAX_FETCH_LINES, TermEvent, TermSize};

    /// A `TermEvent::Lines` body of `count` blank lines claiming `cols` columns each.
    fn blank_lines(cols: u16, count: usize) -> Vec<u8> {
        let mut body =
            codec::encode_body(&TermEvent::Lines { start: LineIndex(0), lines: Vec::new() })
                .unwrap();
        assert_eq!(body.pop(), Some(0), "the empty list's length ends the body");
        body.extend(codec::encode_body(&count).unwrap());
        let line = codec::encode_body(&Line::blank(cols)).unwrap();
        for _ in 0..count {
            body.extend_from_slice(&line);
        }
        body
    }

    #[test]
    fn the_ceilings_hold_the_largest_honest_messages() {
        assert_eq!((MAX_COLS, MAX_ROWS), (2048, 1024));
        assert_eq!(MAX_DECODED_CELLS, 8 << 20, "a full answer to FetchLines at the widest");
        assert_eq!(MAX_DECODED_CELLS, MAX_FETCH_LINES as usize * usize::from(MAX_COLS));
        assert!(MAX_DECODED_CELLS >= usize::from(MAX_COLS) * usize::from(MAX_ROWS), "a screen");
        let bytes = MAX_DECODED_CELLS * size_of::<Cell>();
        assert_eq!(bytes, 384 << 20, "the most one message's lines take: {bytes} bytes");
    }

    /// The input found by reading: 100 blank lines at 65 535 columns, 7 bytes each.
    #[test]
    fn lines_wider_than_any_terminal_do_not_decode() {
        let body = blank_lines(u16::MAX, 100);
        assert_eq!(body.len(), 703);
        let (decoded, spent) =
            with_cell_budget(usize::MAX, || codec::decode_body::<TermEvent>(&body));
        decoded.unwrap_err();
        assert_eq!(spent, 0, "refused at the first line's width, before any cell");
        let widest = codec::decode_body::<TermEvent>(&blank_lines(MAX_COLS, 2)).unwrap();
        assert!(
            matches!(&widest, TermEvent::Lines { lines, .. }
                if lines.iter().all(|l| l.cols() == MAX_COLS)),
            "{widest:?}"
        );
    }

    /// The largest frame a stream carries, every line blank at the widest: the decode stops
    /// at the budget, having put back [`MAX_DECODED_CELLS`] cells, not 2.8 M lines' worth.
    #[test]
    fn a_largest_frame_of_blank_lines_stops_at_the_budget() {
        let per_line = codec::encode_body(&Line::blank(MAX_COLS)).unwrap().len();
        let body = blank_lines(MAX_COLS, (MAX_FRAME_BYTES - 16) / per_line);
        assert!(body.len() <= MAX_FRAME_BYTES && body.len() > MAX_FRAME_BYTES - 16);
        let (decoded, spent) =
            with_cell_budget(usize::MAX, || codec::decode_body::<TermEvent>(&body));
        decoded.unwrap_err();
        assert_eq!(spent, MAX_DECODED_CELLS, "stopped at the budget, not at the frame's end");
    }

    fn frame(cols: u16, rows: u16) -> TermEvent {
        TermEvent::Frame(Frame {
            seq: 1,
            full: true,
            epoch: 0,
            cols,
            rows,
            cursor: Cursor::default(),
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(0),
            total_lines: u64::from(rows),
            input_ack: 0,
            updates: Vec::new(),
            images: Vec::new(),
        })
    }

    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(msg: &T) -> Option<T> {
        codec::decode_body(&codec::encode_body(msg).unwrap()).ok()
    }

    /// A screen past the ceiling is refused: the client makes one of the size a frame or a
    /// `Resized` names, and seven bytes asked it for 4 G cells.
    #[test]
    fn a_screen_past_the_ceiling_does_not_decode() {
        assert!(round_trip(&frame(MAX_COLS, MAX_ROWS)).is_some());
        assert!(round_trip(&frame(MAX_COLS + 1, 1)).is_none());
        assert!(round_trip(&frame(1, MAX_ROWS + 1)).is_none());
        let resized = |cols, rows| TermEvent::Resized { cols, rows };
        assert!(round_trip(&resized(MAX_COLS, MAX_ROWS)).is_some());
        assert!(round_trip(&resized(u16::MAX, 24)).is_none());
        assert!(round_trip(&resized(80, u16::MAX)).is_none());
    }

    /// A size asked for past the ceiling is the ceiling: a window wider than any terminal gets
    /// the widest, and the PTY and the engine are given the same size.
    #[test]
    fn a_size_asked_past_the_ceiling_is_clamped() {
        let metrics = CellMetrics { cell_width: 8, cell_height: 16 };
        let asked = TermSize { cols: u16::MAX, rows: MAX_ROWS + 1, metrics };
        assert_eq!(
            round_trip(&asked),
            Some(TermSize { cols: MAX_COLS, rows: MAX_ROWS, metrics }),
            "clamped"
        );
        let fits = TermSize { cols: 300, rows: 90, metrics };
        assert_eq!(round_trip(&fits), Some(fits));
    }
}
