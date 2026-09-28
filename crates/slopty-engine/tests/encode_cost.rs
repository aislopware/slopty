//! What encoding a terminal frame for the wire costs (`slopty_proto::codec::encode`): the echo
//! of one key, and a whole 200×60 screen as an attach sends it. `cargo xtask bench --filter
//! encode_cost` runs it; `docs/MEASUREMENTS.md`, "Encoding a frame".

#[cfg(test)]
#[expect(clippy::arithmetic_side_effects, reason = "byte counts and rows of small screens")]
mod encode_cost {
    use slopty_engine::{EngineConfig, GhosttyEngine};
    use slopty_proto::codec;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::{TermEvent, TermSize};
    use slopty_testkit::bench::Bench;

    /// An engine whose screen is full of `ls -l`-like output, with the cursor at a prompt.
    fn at_a_full_prompt(cols: u16, rows: u16) -> GhosttyEngine {
        let size = TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } };
        let mut e = GhosttyEngine::new(EngineConfig { size, scrollback_lines: 10_000 }).unwrap();
        for i in 0..u32::from(rows) * 2 {
            let line = format!(
                "-rw-r--r--  1 cong  staff  {:>6} Sep 25 09:{:02} \x1b[34mfile-{i}.rs\x1b[0m\r\n",
                i * 37,
                i % 60
            );
            e.write(line.as_bytes());
        }
        e.write(b"% ");
        e
    }

    #[test]
    #[ignore = "measurement, run by hand"]
    fn encode_cost() {
        let bench = Bench::new("engine.encode_cost");
        let mut e = at_a_full_prompt(80, 24);
        let _attach = e.full_frame(0).unwrap();
        e.write(b"l");
        let echo = TermEvent::Frame(e.take_frame(1).unwrap().expect("an echo dirties its row"));
        let mut series = bench.series("echo");
        for _ in 0..2_000 {
            drop(series.time(|| codec::encode(&echo).unwrap()));
        }
        series.report().unwrap();

        let screen = TermEvent::Frame(at_a_full_prompt(200, 60).full_frame(0).unwrap());
        let mut series = bench.series("full_200x60");
        for _ in 0..500 {
            drop(series.time(|| codec::encode(&screen).unwrap()));
        }
        series.report().unwrap();
    }
}
