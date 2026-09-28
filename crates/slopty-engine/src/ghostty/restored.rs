//! A lost shell's screen, replayed from its last checkpoint, closed off for the new shell that
//! takes the session over (`docs/decisions/terminal.md`, "Sessions come back after a reboot").

use super::GhosttyEngine;
use crate::EngineError;

/// What a lost shell's programs may have left switched on, switched off for a fresh shell: the
/// alternate screen, a soft reset (margins, origin mode, the character sets, the saved
/// cursor), the mouse and focus reports, bracketed paste, application cursor keys and keypad,
/// synchronized output, the kitty keyboard flags, the cursor's look, an open hyperlink and
/// the program's colours.
const RESET: &[u8] = b"\x1b[?1049l\x1b[!p\
\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l\
\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b>\x1b[?2026l\x1b[=0;1u\x1b[?25h\x1b[0 q\x1b[0m\
\x1b]8;;\x1b\\\x1b]104\x1b\\\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\";

/// Box drawings light horizontal, the divider's rule.
const RULE: char = '\u{2500}';

impl GhosttyEngine {
    /// Close off the screen just replayed from a lost shell's checkpoint for the new shell.
    ///
    /// What its programs left on is switched off: the alternate screen, the mouse and focus
    /// reports, bracketed paste, application keys, the kitty keyboard flags, a hidden cursor
    /// and the program's colours. Then a faint divider reading `label` goes on the row below
    /// the last one with text, scrolling the screen up if that is past the bottom. The cursor
    /// ends at the start of the row below the divider, where the new shell's prompt goes.
    ///
    /// The divider goes below the text rather than at the cursor: a program drawn in place
    /// (Claude Code's input box) leaves the cursor above its last rows.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn mark_restored(&mut self, label: &str) -> Result<(), EngineError> {
        self.write(RESET);
        let screen = self.screen_text()?;
        let below = match screen.rows.iter().rposition(|row| !row.trim().is_empty()) {
            Some(last) => format!("\x1b[{};1H\r\n", last.saturating_add(1)),
            None => "\x1b[H".to_owned(),
        };
        self.write(below.as_bytes());
        let cols = usize::from(self.size.cols);
        let text = format!("{RULE}{RULE} {label} ");
        let mut line: String = text.chars().take(cols).collect();
        let rest = cols.saturating_sub(line.chars().count());
        line.extend(std::iter::repeat_n(RULE, rest));
        self.write(format!("\x1b[2m{line}\x1b[0m\r\n").as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::TermModes;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;

    use super::*;
    use crate::{EngineConfig, EngineEvent};

    const LABEL: &str = "Restored after restart";

    fn engine(cols: u16, rows: u16) -> GhosttyEngine {
        GhosttyEngine::new(EngineConfig {
            size: TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } },
            scrollback_lines: 100,
        })
        .unwrap()
    }

    /// What a checkpoint of `e` replays into a fresh engine of its size.
    fn replayed(e: &mut GhosttyEngine) -> GhosttyEngine {
        let mut state = Vec::new();
        e.checkpoint(&mut state).unwrap();
        let mut fresh = engine(e.size.cols, e.size.rows);
        fresh.write(&state);
        fresh
    }

    fn rows(e: &GhosttyEngine) -> Vec<String> {
        e.screen_text().unwrap().rows
    }

    fn divider(cols: usize) -> String {
        let text = format!("── {LABEL} ");
        format!("{text}{}", "─".repeat(cols.saturating_sub(text.chars().count())))
    }

    /// A shell's screen comes back with the divider below its last text and the new prompt
    /// below that; what the old programs switched on is off, and the colours are the defaults.
    #[test]
    fn the_divider_goes_below_the_old_screen_and_the_modes_are_reset() {
        let mut old = engine(30, 6);
        old.write(b"$ make\r\nbuilt\r\n$ ");
        old.write(
            b"\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?1h\x1b[?25l\x1b]11;#102030\x07",
        );
        let mut e = replayed(&mut old);
        let _replay = e.drain_events();
        e.mark_restored(LABEL).unwrap();
        e.write(b"$ ");
        assert_eq!(rows(&e), ["$ make", "built", "$", &divider(30), "$", ""]);
        let screen = e.screen_text().unwrap();
        assert_eq!(screen.cursor, (4, 2));
        let modes = e.full_frame(0).unwrap().modes;
        let off = TermModes::MOUSE_TRACKING
            | TermModes::BRACKETED_PASTE
            | TermModes::FOCUS_EVENTS
            | TermModes::APP_CURSOR_KEYS
            | TermModes::CURSOR_HIDDEN
            | TermModes::ALT_SCREEN;
        assert!(!modes.intersects(off), "{modes:?}");
        assert!(
            e.drain_events()
                .iter()
                .any(|ev| matches!(ev, EngineEvent::Colors(c) if c.bg.is_none())),
            "the old program's background goes"
        );
        let full = e.full_frame(0).unwrap();
        let rule = &full.updates[3].line;
        assert!(rule.cells[0].style.flags.contains(slopty_grid::StyleFlags::FAINT), "faint");
    }

    /// A program drawn in place leaves the cursor in its input box with a status row below:
    /// the divider goes under the status row, not over it.
    #[test]
    fn the_divider_goes_below_rows_under_the_cursor() {
        let mut e = engine(30, 6);
        e.write(b"> fix the bug\r\n  status: idle\x1b[1;3H");
        e.mark_restored(LABEL).unwrap();
        assert_eq!(rows(&e)[..3], ["> fix the bug", "  status: idle", &divider(30)]);
    }

    /// A full screen scrolls up to make room: its top goes to the scrollback, nothing is
    /// overwritten.
    #[test]
    fn a_full_screen_scrolls_to_make_room() {
        let mut e = engine(30, 3);
        e.write(b"one\r\ntwo\r\nthree");
        e.mark_restored(LABEL).unwrap();
        e.write(b"$ ");
        assert_eq!(rows(&e), ["three", &divider(30), "$"]);
        let history = e.text_lines(None, 10).unwrap().lines;
        assert_eq!(history, ["one", "two", "three", &divider(30), "$"]);
    }

    /// A lost shell that was on the alternate screen (a full-screen program) comes back on
    /// the primary one, its scrollback intact, and a narrow terminal still gets a whole rule.
    #[test]
    fn the_alternate_screen_is_left_and_a_narrow_rule_is_cut() {
        let mut old = engine(12, 4);
        old.write(b"$ vim\r\n\x1b[?1049h\x1b[2Jediting");
        let mut e = replayed(&mut old);
        e.mark_restored(LABEL).unwrap();
        let screen = e.screen_text().unwrap();
        assert!(!screen.alternate);
        assert_eq!(screen.rows[..2], ["$ vim", "── Restored"]);
        assert_eq!(screen.rows[1].chars().count(), 11, "trailing blank trimmed, 12 columns");
    }

    /// A lost shell with an empty screen gets the divider on its first row.
    #[test]
    fn an_empty_screen_gets_the_divider_on_top() {
        let mut e = engine(30, 3);
        e.mark_restored(LABEL).unwrap();
        assert_eq!(rows(&e), [divider(30), String::new(), String::new()]);
    }
}
