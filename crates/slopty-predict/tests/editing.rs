//! A line editor at a shell prompt, replayed over a shaped round trip: which keys the local echo
//! shows as they are pressed, how long each key takes to look right, and whether the overlay
//! ever shows the line as the shell never has it.
//!
//! The shell is modelled on zle: insert mode, ← → within the buffer, ⌫ deleting before the
//! cursor, and in the second session a zsh-autosuggestions suggestion in bright black after the
//! buffer, which → at the end of the buffer accepts. The link delivers each key half a round
//! trip after it is pressed, and the frame answering it half a round trip later. The numbers
//! print with `--nocapture` (MEASUREMENTS, "prediction over the line editor's keys").

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "a simulation over small counts and milliseconds"
)]
mod editing {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_grid::{
        Cell, Color, Cursor, CursorShape, Line, RowUpdate, Screen, SemanticMark, Style, TermModes,
    };
    use slopty_predict::{Policy, Predictor};
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};

    const COLS: u16 = 80;
    const PROMPT: &str = "$ ";
    const TYPING: u64 = 66;
    const HELD: u64 = 33;
    const PAUSE: u64 = 400;
    /// The longest buffer the model draws on one row, past the right prompt's place.
    const LONGEST: usize = 72;

    #[derive(Clone, Copy, Debug)]
    enum Key {
        Type(char),
        Left,
        Right,
        Erase,
    }

    /// A zle-like line editor on row 0.
    #[derive(Clone, Debug)]
    struct Shell {
        buffer: Vec<char>,
        cursor: usize,
        history: Option<&'static str>,
        /// A right prompt in blue, drawn while the line leaves room for it (zsh: the line and
        /// the prompt short of the last column).
        rprompt: Option<&'static str>,
    }

    impl Shell {
        const fn new(history: Option<&'static str>, rprompt: Option<&'static str>) -> Self {
            Self { buffer: Vec::new(), cursor: 0, history, rprompt }
        }

        fn press(&mut self, key: Key) {
            match key {
                Key::Type(c) => {
                    self.buffer.insert(self.cursor, c);
                    self.cursor += 1;
                }
                Key::Left => self.cursor = self.cursor.saturating_sub(1),
                Key::Right if self.cursor < self.buffer.len() => self.cursor += 1,
                Key::Right => {
                    if let Some(rest) = self.suggestion() {
                        self.buffer.extend(rest.chars());
                        self.cursor = self.buffer.len();
                    }
                }
                Key::Erase => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        self.buffer.remove(self.cursor);
                    }
                }
            }
        }

        fn typed(&self) -> String {
            self.buffer.iter().collect()
        }

        fn suggestion(&self) -> Option<String> {
            let typed = self.typed();
            let rest = self.history?.strip_prefix(typed.as_str())?;
            (!typed.is_empty() && !rest.is_empty()).then(|| rest.to_owned())
        }

        fn screen(&self) -> Screen {
            let mut line = Line::blank(COLS);
            let ghost = Style { fg: Color::Palette(8), ..Style::DEFAULT };
            let text =
                PROMPT.chars().chain(self.buffer.iter().copied()).map(|c| (c, Style::DEFAULT));
            let rest = self.suggestion().unwrap_or_default();
            for (cell, (c, style)) in
                line.cells.iter_mut().zip(text.chain(rest.chars().map(|c| (c, ghost))))
            {
                *cell = Cell::narrow(c, style);
            }
            if let Some(right) = self.rprompt {
                let start = usize::from(COLS) - 1 - right.len();
                if PROMPT.len() + self.buffer.len() + rest.len() < start {
                    let blue = Style { fg: Color::Palette(4), ..Style::DEFAULT };
                    for (cell, c) in line.cells.iter_mut().skip(start).zip(right.chars()) {
                        *cell = Cell::narrow(c, blue);
                    }
                }
            }
            let input = (!self.buffer.is_empty()).then_some(2);
            line.mark = SemanticMark::Prompt { exit: None, input };
            let mut screen = Screen::new(COLS, 24);
            screen.apply(RowUpdate { row: 0, line: Arc::new(line) }).unwrap();
            *screen.cursor_mut() = self.cursor();
            screen
        }

        fn cursor(&self) -> Cursor {
            let col = u16::try_from(PROMPT.len() + self.cursor).unwrap();
            Cursor { row: 0, col, shape: CursorShape::Bar, visible: true, blink: false }
        }

        /// The line as a reader takes it: the prompt and the buffer, and where the cursor is.
        fn truth(&self) -> (String, u16) {
            (format!("{PROMPT}{}", self.typed()).trim_end().to_owned(), self.cursor().col)
        }
    }

    /// What the client shows: the last frame with the guesses drawn over it. A suggestion's
    /// bright black and the right prompt's blue read as blank: the reader skips them.
    fn shown(screen: &Screen, p: &Predictor, now: Instant) -> (String, u16) {
        let line = screen.line(0).unwrap();
        let mut chars: Vec<char> = line
            .cells
            .iter()
            .map(|cell| {
                let skipped = matches!(cell.style.fg, Color::Palette(8 | 4));
                cell.text.as_str().chars().next().filter(|_| !skipped).unwrap_or(' ')
            })
            .collect();
        let real = screen.cursor();
        if !p.visible(now) {
            return (chars.iter().collect::<String>().trim_end().to_owned(), real.col);
        }
        for guess in p.pending().iter().filter(|g| g.row == 0) {
            chars[usize::from(guess.col)] = guess.text.chars().next().unwrap_or(' ');
        }
        (chars.iter().collect::<String>().trim_end().to_owned(), p.cursor(real).col)
    }

    fn key(seq: u64, key: Key) -> KeyEvent {
        let (code, text) = match key {
            Key::Type(c) => (KeyCode::A, Some(c.to_string())),
            Key::Left => (KeyCode::ArrowLeft, None),
            Key::Right => (KeyCode::ArrowRight, None),
            Key::Erase => (KeyCode::Backspace, None),
        };
        KeyEvent {
            seq,
            action: KeyAction::Press,
            code,
            mods: Mods::empty(),
            consumed_mods: Mods::empty(),
            unshifted: text.as_deref().and_then(|t| t.chars().next()),
            text,
            composing: false,
            option_as_alt: false,
        }
    }

    /// Keys with the pause before each, in ms.
    fn script(steps: &[(&str, u64)]) -> Vec<(Key, u64)> {
        let mut keys = Vec::new();
        for (step, gap) in steps {
            let mut first = true;
            let mut push = |k: Key, gap: u64| {
                keys.push((k, if first { PAUSE } else { gap }));
                first = false;
            };
            if let Some(n) = step.strip_prefix('<') {
                (0..n.parse::<u32>().unwrap()).for_each(|_| push(Key::Left, *gap));
            } else if let Some(n) = step.strip_prefix('>') {
                (0..n.parse::<u32>().unwrap()).for_each(|_| push(Key::Right, *gap));
            } else if let Some(n) = step.strip_prefix('#') {
                (0..n.parse::<u32>().unwrap()).for_each(|_| push(Key::Erase, *gap));
            } else {
                step.chars().for_each(|c| push(Key::Type(c), *gap));
            }
        }
        keys
    }

    /// Fixing a typo in the middle of a line, then rewriting its end.
    fn fixing_a_typo() -> Vec<(Key, u64)> {
        script(&[
            ("echo halo world", TYPING),
            ("<7", HELD),
            ("l", TYPING),
            (">7", HELD),
            ("#5", TYPING),
            ("there", TYPING),
            ("<5", HELD),
            ("#1", TYPING),
            ("-", TYPING),
        ])
    }

    /// A suggestion taken with → and then edited.
    fn taking_a_suggestion() -> Vec<(Key, u64)> {
        script(&[
            ("git co", TYPING),
            (">1", HELD),
            ("<7", HELD),
            ("#3", TYPING),
            ("fixes", TYPING),
            (">7", HELD),
            ("#2", TYPING),
        ])
    }

    #[derive(Debug, Default)]
    struct Run {
        keys: usize,
        at_press: usize,
        wrong: usize,
        misses: u32,
        right_ms: Vec<u64>,
    }

    fn replay(keys: &[(Key, u64)], shell: Shell, rtt: Duration) -> Run {
        let t0 = Instant::now();
        let mut p = Predictor::new(Policy::Adaptive);
        p.set_rtt(Some(rtt));
        let mut worker = shell;
        let mut client = worker.screen();
        let _r = p.on_frame(&client, 0, 0, t0);
        // (arrival, ack, screen) of the frames in flight, in order.
        let mut link: Vec<(Instant, u64, Screen)> = Vec::new();
        // Per key: when pressed and the truth after it; when it first looked right.
        let mut truths: Vec<(Instant, (String, u16))> = Vec::new();
        let mut right: Vec<Option<Instant>> = Vec::new();
        let mut run = Run::default();
        let mut at = t0 + Duration::from_millis(PAUSE);
        let mut seq = 0;
        let settle = |shown: &(String, u16),
                      now: Instant,
                      truths: &[(Instant, (String, u16))],
                      right: &mut [Option<Instant>]| {
            for i in 0..truths.len() {
                if right[i].is_none() && truths[i..].iter().any(|(_, t)| t == shown) {
                    right[i] = Some(now);
                }
            }
        };
        let start = worker.truth();
        // The line as the shell never had it, up to the keys pressed so far.
        let never = |shown: &(String, u16), truths: &[(Instant, (String, u16))]| {
            *shown != start && truths.iter().all(|(_, t)| t != shown)
        };
        let mut events = keys.iter().peekable();
        let mut stopped = false;
        loop {
            let next_key = if stopped {
                None
            } else {
                events.peek().map(|(_, gap)| at + Duration::from_millis(*gap))
            };
            let next_frame = link.first().map(|(t, ..)| *t);
            let key_first = match (next_key, next_frame) {
                (None, None) => break,
                (Some(k), Some(f)) => k < f,
                (Some(_), None) => true,
                (None, Some(_)) => false,
            };
            if key_first {
                let (pressed, _) = events.next().unwrap();
                if matches!(pressed, Key::Type(_)) && worker.buffer.len() >= LONGEST {
                    // The model does not wrap: a session ends before its line would.
                    stopped = true;
                    continue;
                }
                at = next_key.unwrap();
                seq += 1;
                let _guess =
                    p.on_key(&key(seq, *pressed), client.cursor(), COLS, TermModes::ECHO_OFF, at);
                worker.press(*pressed);
                link.push((at + rtt, seq, worker.screen()));
                truths.push((at, worker.truth()));
                right.push(None);
                let now_shown = shown(&client, &p, at);
                if now_shown == worker.truth() {
                    run.at_press += 1;
                }
                run.wrong += usize::from(never(&now_shown, &truths));
                settle(&now_shown, at, &truths, &mut right);
            } else {
                let (arrived, ack, screen) = link.remove(0);
                client = screen;
                run.misses += p.on_frame(&client, ack, 0, arrived).misses;
                let now_shown = shown(&client, &p, arrived);
                run.wrong += usize::from(never(&now_shown, &truths));
                settle(&now_shown, arrived, &truths, &mut right);
            }
        }
        run.keys = truths.len();
        for ((pressed, _), right) in truths.iter().zip(&right) {
            let right = right.unwrap_or(*pressed + Duration::from_secs(10));
            run.right_ms.push(u64::try_from(right.duration_since(*pressed).as_millis()).unwrap());
        }
        run
    }

    fn row(label: &str, rtt: Duration, run: &Run) -> String {
        let mut sorted = run.right_ms.clone();
        sorted.sort_unstable();
        let pct = |q: usize| sorted[(sorted.len() - 1) * q / 100];
        let mean = sorted.iter().sum::<u64>() as f64 / sorted.len() as f64;
        format!(
            "| {label} | {} ms | {} of {} | {:.1} | {} / {} | {} | {} |",
            rtt.as_millis(),
            run.at_press,
            run.keys,
            mean,
            pct(50),
            pct(95),
            run.wrong,
            run.misses
        )
    }

    /// Over 20 and 40 ms, every ← → ⌫ inside the buffer is drawn as it is pressed after the
    /// warm-up, → at the end of the buffer (a suggestion taken) is left to the shell, and the
    /// overlay never shows a line the shell does not have.
    #[test]
    fn editing_keys_show_as_pressed_over_a_round_trip() {
        println!(
            "| session | rtt | keys right at press | key → right, mean ms | p50 / p95 ms | wrong overlays | misses |"
        );
        println!("| --- | --- | --- | --- | --- | --- | --- |");
        for rtt in [Duration::from_millis(20), Duration::from_millis(40)] {
            let typo = replay(&fixing_a_typo(), Shell::new(None, Some("~/src/slopty")), rtt);
            let history = Some("git commit -m 'fix the build'");
            let suggested = replay(&taking_a_suggestion(), Shell::new(history, None), rtt);
            println!("{}", row("fixing a typo", rtt, &typo));
            println!("{}", row("taking a suggestion", rtt, &suggested));
            for (name, run) in [("typo", &typo), ("suggestion", &suggested)] {
                assert_eq!((run.wrong, run.misses), (0, 0), "{name} at {rtt:?}: {run:?}");
            }
            // The first two keys warm the predictor up. The → that takes the suggestion is
            // the shell's to draw; the epoch after it is tentative until an echo, which at
            // 40 ms is two repeats of a held key.
            assert!(typo.at_press + 2 >= typo.keys, "typo at {rtt:?}: {typo:?}");
            let taken = if rtt > Duration::from_millis(33) { 5 } else { 4 };
            assert!(
                suggested.at_press + taken >= suggested.keys,
                "suggestion at {rtt:?}: {suggested:?}"
            );
        }
    }

    /// xorshift64, so every run replays the same sessions.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x % n
        }
    }

    /// Random sessions of typing, ← → and ⌫ at random paces over round trips of 5 to 85 ms,
    /// half of them with a suggestion from history and half with a right prompt the line grows
    /// into: no guess is ever wrong, and the overlay never shows a line the shell does not have.
    #[test]
    fn random_editing_never_shows_a_wrong_line() {
        let mut rng = Rng(0x5eed_1234_abcd_ef01);
        let alphabet: Vec<char> = "git cm-'fxhb".chars().collect();
        for session in 0..2000 {
            let rtt = Duration::from_millis(5 + rng.below(80));
            let history = (rng.below(2) == 0).then_some("git commit -m 'fix the build'");
            let len = 10 + rng.below(110);
            let keys: Vec<(Key, u64)> = std::iter::repeat_with(|| {
                let key = match rng.below(10) {
                    0..=5 => Key::Type(alphabet[rng.below(alphabet.len() as u64) as usize]),
                    6 | 7 => Key::Left,
                    8 => Key::Right,
                    _ => Key::Erase,
                };
                (key, 10 + rng.below(140))
            })
            .take(len as usize)
            .collect();
            let rprompt = (rng.below(2) == 0).then_some("~/src main");
            let run = replay(&keys, Shell::new(history, rprompt), rtt);
            assert_eq!((run.wrong, run.misses), (0, 0), "session {session} at {rtt:?}: {keys:?}");
        }
    }
}
