//! The diffs a viewer is sent, applied the way a client applies them (lines kept by their
//! absolute index in each numbering, the screen read back from them), always show what the
//! terminal shows: through scrolls, scroll regions, erases, the alternate screen and joiners.

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "index and generator arithmetic on small, bounded values"
)]
mod diffs {
    use std::collections::BTreeMap;

    use slopty_engine::{EngineConfig, GhosttyEngine};
    use slopty_grid::Line;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::{Frame, TermSize};

    /// A viewer: every line it was sent, by numbering and absolute index.
    #[derive(Default)]
    struct Viewer {
        held: BTreeMap<(u32, u64), Line>,
        rows_sent: usize,
    }

    impl Viewer {
        fn apply(&mut self, frame: &Frame) {
            let first = frame.first_visible_line.0;
            for u in &frame.updates {
                self.held.insert((frame.epoch, first + u64::from(u.row)), u.line.clone());
            }
            self.rows_sent += frame.updates.len();
        }

        /// The screen as this viewer shows it after `frame`.
        fn screen(&self, frame: &Frame) -> Vec<Option<String>> {
            (0..u64::from(frame.rows))
                .map(|y| {
                    self.held
                        .get(&(frame.epoch, frame.first_visible_line.0 + y))
                        .map(|l| format!("{l:?}"))
                })
                .collect()
        }
    }

    fn engine(cols: u16, rows: u16) -> GhosttyEngine {
        let size = TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } };
        GhosttyEngine::new(EngineConfig { size, scrollback_lines: 200 }).unwrap()
    }

    fn truth(frame: &Frame) -> Vec<Option<String>> {
        assert!(frame.full);
        frame.updates.iter().map(|u| Some(format!("{:?}", u.line))).collect()
    }

    /// A small deterministic generator, so a failure replays.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n
        }
    }

    /// Output a shell, `ls`, a pager and a TUI would write.
    fn chunk(rng: &mut Lcg, alt: &mut bool) -> Vec<u8> {
        let word = |rng: &mut Lcg| -> String {
            let len = rng.next(12) + 1;
            (0..len)
                .map(|i| char::from(b'a' + u8::try_from((rng.next(26) + i) % 26).unwrap()))
                .collect()
        };
        match rng.next(16) {
            0..=4 => format!("{} {}\r\n", word(rng), word(rng)).into_bytes(),
            5 => format!("\x1b[3{}m{}\x1b[0m", rng.next(8), word(rng)).into_bytes(),
            6 => b"\x1b]133;D;0\x07\x1b]133;A\x07% \x1b]133;B\x07".to_vec(),
            7 => b"\r\n\x1b]133;C\x07".to_vec(),
            8 => format!("\x1b[{};{}H", rng.next(8) + 1, rng.next(30) + 1).into_bytes(),
            9 => b"\x1b[K".to_vec(),
            10 => format!("\x1b[2;{}r\x1b[{}H\r\n\r\nx\x1b[r", rng.next(5) + 3, rng.next(5) + 2)
                .into_bytes(),
            11 => "字\u{1F600}é\r\n".as_bytes().to_vec(),
            12 => b"\x1b[2J\x1b[H".to_vec(),
            13 => {
                *alt = !*alt;
                if *alt { b"\x1b[?1049h\x1b[H".to_vec() } else { b"\x1b[?1049l".to_vec() }
            }
            14 => (0..rng.next(30)).flat_map(|i| format!("line {i}\r\n").into_bytes()).collect(),
            _ => word(rng).into_bytes(),
        }
    }

    /// Someone joins: every row for them, nothing taken from the others.
    fn join(subject: &mut GhosttyEngine) -> Viewer {
        let (frame, _) = subject.join_frame(0).unwrap();
        let mut joiner = Viewer::default();
        joiner.apply(&frame);
        joiner
    }

    #[test]
    fn every_diff_applied_by_index_shows_the_terminal() {
        for seed in 1..=12_u64 {
            let mut rng = Lcg(seed);
            let mut joins = Lcg(seed.wrapping_add(1_000));
            let (mut subject, mut mirror) = (engine(30, 8), engine(30, 8));
            let mut viewers = vec![Viewer::default()];
            viewers[0].apply(&subject.full_frame(0).unwrap());
            let _first = mirror.full_frame(0).unwrap();
            let mut alt = false;
            let (mut diffs, mut full_diffs) = (0_u32, 0_u32);
            for step in 0..600 {
                let bytes = chunk(&mut rng, &mut alt);
                subject.write(&bytes);
                mirror.write(&bytes);
                if rng.next(3) == 0 {
                    // Nobody takes a diff; someone may join meanwhile (a separate generator,
                    // so the output stays the same for a seed).
                    if joins.next(4) == 0 {
                        viewers.push(join(&mut subject));
                    }
                    continue;
                }
                if rng.next(40) == 0 {
                    viewers.push(join(&mut subject));
                }
                let Some(frame) = subject.take_frame(0).unwrap() else { continue };
                diffs += 1;
                full_diffs += u32::from(frame.full);
                let expected = truth(&mirror.full_frame(0).unwrap());
                for (n, v) in viewers.iter_mut().enumerate() {
                    v.apply(&frame);
                    assert_eq!(v.screen(&frame), expected, "seed {seed} step {step} viewer {n}");
                }
            }
            assert!(diffs > 200, "seed {seed}: {diffs} frames");
            assert!(
                full_diffs < diffs / 4,
                "seed {seed}: {full_diffs} of {diffs} frames were full"
            );
        }
    }

    /// Output scrolling a full screen: each frame carries the lines that came in, not the screen.
    #[test]
    fn a_scroll_sends_the_lines_that_came_in() {
        let mut e = engine(40, 10);
        let mut viewer = Viewer::default();
        for i in 0..30 {
            e.write(format!("before {i}\r\n").as_bytes());
        }
        viewer.apply(&e.full_frame(0).unwrap());
        let sent = viewer.rows_sent;
        for i in 0..3 {
            e.write(format!("after {i}\r\n").as_bytes());
        }
        let f = e.take_frame(0).unwrap().expect("output");
        assert!(!f.full, "a scroll in the same numbering");
        viewer.apply(&f);
        let texts: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
        assert_eq!(texts, ["after 0", "after 1", "after 2", ""], "{f:?}");
        assert_eq!(viewer.rows_sent - sent, 4);
    }

    /// A line that changed after the others were sent it, and changed back after a viewer joined:
    /// the joiner holds the middle version, so the next scroll sends the line though the others
    /// hold it as it is.
    #[test]
    fn a_joiner_is_sent_a_line_that_changed_back_since_the_others_had_it() {
        let (mut e, mut mirror) = (engine(20, 4), engine(20, 4));
        let both = |e: &mut GhosttyEngine, mirror: &mut GhosttyEngine, bytes: &[u8]| {
            e.write(bytes);
            mirror.write(bytes);
        };
        both(&mut e, &mut mirror, b"l0\r\nA\r\nl2\r\nl3");
        let mut others = Viewer::default();
        others.apply(&e.full_frame(0).unwrap());
        let _first = mirror.full_frame(0).unwrap();
        both(&mut e, &mut mirror, b"\x1b[2;1HB\x1b[4;3H");
        let (join, _) = e.join_frame(0).unwrap();
        let mut joiner = Viewer::default();
        joiner.apply(&join);
        both(&mut e, &mut mirror, b"\x1b[2;1HA\x1b[4;3H\r\n");
        let f = e.take_frame(0).unwrap().expect("the scroll");
        assert!(!f.full);
        others.apply(&f);
        joiner.apply(&f);
        let expected = truth(&mirror.full_frame(0).unwrap());
        assert_eq!(others.screen(&f), expected);
        assert_eq!(joiner.screen(&f), expected);
    }

    /// A program takes the alternate screen while no viewer takes a diff, and a viewer joins
    /// there: it holds nothing of the primary's numbering, and a viewer that missed the diffs
    /// before holds an older version of it. The primary coming back is sent whole.
    #[test]
    fn a_viewer_that_joined_on_the_alternate_screen_is_sent_the_primary_whole() {
        let (mut e, mut mirror) = (engine(20, 4), engine(20, 4));
        let both = |e: &mut GhosttyEngine, mirror: &mut GhosttyEngine, bytes: &[u8]| {
            e.write(bytes);
            mirror.write(bytes);
        };
        both(&mut e, &mut mirror, b"l0\r\nl1\r\nl2\r\n$ ");
        let mut behind = Viewer::default();
        behind.apply(&e.full_frame(0).unwrap());
        let _first = mirror.full_frame(0).unwrap();
        both(&mut e, &mut mirror, b"\x1b[1;1Hm0\x1b[4;3H");
        // Taken by the others, missed by `behind`.
        let _edit = e.take_frame(0).unwrap().expect("the edit");
        both(&mut e, &mut mirror, b"less\r\n\x1b[?1049h\x1b[Hpage");
        let (join, _) = e.join_frame(0).unwrap();
        let mut joiner = Viewer::default();
        joiner.apply(&join);
        behind.apply(&join);
        both(&mut e, &mut mirror, b"\x1b[?1049l");
        let f = e.take_frame(0).unwrap().expect("the primary again");
        let expected = truth(&mirror.full_frame(0).unwrap());
        for (who, v) in [("joiner", &mut joiner), ("behind", &mut behind)] {
            v.apply(&f);
            assert_eq!(v.screen(&f), expected, "{who}, full {}", f.full);
        }
    }
}
