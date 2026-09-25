//! What a keystroke's echo and an Enter at a bottom prompt cost on the wire: the encoded
//! `TermEvent::Frame` a viewer following the diffs is sent. `cargo nextest run -p
//! slopty-engine --test wire_bytes --no-capture` prints the numbers (`docs/MEASUREMENTS.md`,
//! 2026-09-25 "a scroll ships the rows it moved").

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "byte counts and timings of a few hundred frames"
)]
mod wire {
    use slopty_engine::{EngineConfig, GhosttyEngine};
    use slopty_proto::codec;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::{Frame, TermEvent, TermSize};

    /// A zsh prompt with the marks slopty's integration writes: status, start, then the input mark.
    fn prompt(status: u8) -> Vec<u8> {
        format!(
            "\x1b]133;D;{status}\x07\x1b]133;A\x07\x1b[1m~/src/slopty\x1b[0m on \x1b[35mmain\x1b[0m % \
             \x1b]133;B\x07"
        )
        .into_bytes()
    }

    /// An engine of `cols` × `rows` whose screen is full of `ls -l`-like output, the cursor at a
    /// prompt on the bottom row, and the attach frame already taken.
    fn at_a_full_prompt(cols: u16, rows: u16) -> GhosttyEngine {
        let size = TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } };
        let mut e = GhosttyEngine::new(EngineConfig { size, scrollback_lines: 10_000 }).unwrap();
        e.write(&prompt(0));
        for i in 0..u32::from(rows) * 2 {
            e.write(
                format!(
                    "-rw-r--r--  1 cong  staff  {:>6} Sep 25 09:{:02} \x1b[34mfile-{i}.rs\x1b[0m\r\n",
                    i * 37,
                    i % 60
                )
                .as_bytes(),
            );
        }
        e.write(&prompt(0));
        let _attach = e.full_frame(0).unwrap();
        e
    }

    fn bytes(frame: Frame) -> usize {
        codec::encode(&TermEvent::Frame(frame)).unwrap().len()
    }

    /// Type `ls` at the prompt, then Enter: the echo of one key, and the frame after the command
    /// printed three lines and the next prompt came up at the bottom.
    fn measure(cols: u16, rows: u16) -> (usize, usize, Frame) {
        let mut e = at_a_full_prompt(cols, rows);
        e.write(b"l");
        let echo = bytes(e.take_frame(1).unwrap().expect("the echo"));
        e.write(b"s");
        let _second = e.take_frame(2).unwrap();
        e.write(b"\r\n\x1b]133;C\x07");
        e.write(b"Cargo.toml  crates  docs\r\napps  vendor  xtask\r\nREADME.md\r\n");
        e.write(&prompt(0));
        let enter = e.take_frame(3).unwrap().expect("the command's frame");
        let enter_bytes = bytes(enter.clone());
        (echo, enter_bytes, enter)
    }

    #[test]
    fn an_echo_and_an_enter_cost_what_changed() {
        for (cols, rows) in [(80_u16, 24_u16), (200, 60)] {
            let (echo, enter, frame) = measure(cols, rows);
            eprintln!(
                "{cols}x{rows}: echo {echo} B, enter {enter} B ({} rows, full {})",
                frame.updates.len(),
                frame.full
            );
            assert!(echo < 300, "a row travels to its last cell: {echo} B");
            assert!(!frame.full && frame.updates.len() == 4, "the rows that came in: {frame:?}");
            assert!(enter < 1_000, "{enter} B");
        }
    }

    /// What building and encoding a one-line scroll costs at 200 × 60 (run it `--release`): every
    /// row is read again either way, and now each is hashed where before each was encoded.
    #[test]
    fn a_one_line_scroll_costs_time_to_build() {
        const LINES: u32 = 500;
        let mut e = at_a_full_prompt(200, 60);
        let mut spent = std::time::Duration::ZERO;
        let mut sent = 0_usize;
        for i in 0..LINES {
            e.write(
                format!("-rw-r--r--  1 cong  staff  {i:>6} Sep 25 09:00 file-{i}.rs\r\n")
                    .as_bytes(),
            );
            let from = std::time::Instant::now();
            let frame = e.take_frame(0).unwrap().expect("a line");
            sent += codec::encode(&TermEvent::Frame(frame)).unwrap().len();
            spent += from.elapsed();
        }
        eprintln!(
            "200x60 one-line scroll: {:.0} us to build and encode, {} B a frame",
            spent.as_secs_f64() * 1e6 / f64::from(LINES),
            sent / LINES as usize
        );
        assert!(sent / (LINES as usize) < 1_000, "one line, not the screen");
    }
}
