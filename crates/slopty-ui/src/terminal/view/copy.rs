//! Keyboard copy mode in the view (`docs/decisions/terminal.md`, "Keyboard copy mode"): the
//! keys [`copy_mode`] reads go to it rather than the program, the viewport follows its cursor,
//! and the foot of the grid says the mode is on, with a way out a touch can reach.
//!
//! Everything a key does here reads the client's mirror of the grid: no key waits on the worker.
//! A line the viewport scrolls to that is not held yet is fetched as any scroll fetches it.

use gpui::{Context, Keystroke, SharedString, Window};
use slopty_client::Effect;
use slopty_grid::LineIndex;

use super::{CopyMode, FootPill, TerminalView};
use crate::terminal::copy_mode::{self, Command, Mode};

/// What the foot of the grid says while copy mode is on.
pub const COPY_MODE: &str = "Copy mode";
/// The way out of it, on the same pill: Esc's, for a touch.
pub const COPY_MODE_DONE: &str = "Done";

impl TerminalView {
    /// Keyboard copy mode on (`CopyMode`): the cursor starts at the shell's while the view
    /// follows the output, at the viewport's foot when it is scrolled back, or takes over the
    /// selection the pointer made (its moving end). On already, nothing changes.
    pub fn copy_mode(&mut self, _: &CopyMode, _window: &mut Window, cx: &mut Context<Self>) {
        if self.copy_mode.is_some() {
            return;
        }
        let mode = self.selection.map_or_else(|| Mode::at(self.copy_start()), Mode::adopting);
        self.copy_mode = Some(mode);
        self.marked = None;
        self.follow_copy_cursor(0, cx);
    }

    /// Whether keyboard copy mode is on.
    #[must_use]
    pub const fn in_copy_mode(&self) -> bool {
        self.copy_mode.is_some()
    }

    /// Where copy mode's cursor is, while it is on: the element draws it.
    #[must_use]
    pub fn copy_cursor(&self) -> Option<(LineIndex, u16)> {
        self.copy_mode.map(|mode| mode.cursor())
    }

    /// The cell copy mode starts on with nothing selected.
    const fn copy_start(&self) -> (LineIndex, u16) {
        if self.state.view_offset() == 0 {
            let cursor = self.state.cursor();
            (self.state.index_at_row(cursor.row), cursor.col)
        } else {
            (self.state.index_at_row(self.state.size().rows.saturating_sub(1)), 0)
        }
    }

    /// A key in copy mode: what [`copy_mode::command`] makes of it. `window` is `None` from
    /// the phone's key bar, where `/` cannot be typed.
    pub(super) fn copy_key(
        &mut self,
        keystroke: &Keystroke,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.copy_command(copy_mode::command(keystroke), window, cx);
    }

    /// Do what a key in copy mode asks.
    pub(super) fn copy_command(
        &mut self,
        command: Command,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut mode) = self.copy_mode else { return };
        match command {
            Command::Move(motion) => {
                let scroll = mode.move_by(motion, &self.state);
                self.copy_mode = Some(mode);
                self.follow_copy_cursor(scroll, cx);
            }
            Command::Select(kind) => {
                mode.select(kind);
                self.copy_mode = Some(mode);
                self.follow_copy_cursor(0, cx);
            }
            Command::SwapEnds => {
                mode.swap_ends();
                self.copy_mode = Some(mode);
                self.follow_copy_cursor(0, cx);
            }
            Command::Copy => {
                if self.selection.is_some() {
                    self.copy_selection(cx);
                    self.leave_copy_mode(cx);
                }
            }
            Command::Escape if mode.selecting() => {
                mode.drop_selection();
                self.copy_mode = Some(mode);
                self.follow_copy_cursor(0, cx);
            }
            Command::Escape | Command::Leave => self.leave_copy_mode(cx),
            Command::Find => {
                if let Some(window) = window {
                    self.find(&super::Find, window, cx);
                }
            }
            Command::NextHit => self.step_match(1, cx),
            Command::PrevHit => self.step_match(-1, cx),
            Command::Nothing => {}
        }
    }

    /// The search went to a hit: in copy mode the cursor goes to its first cell.
    pub(super) fn copy_cursor_to_hit(&mut self, cx: &mut Context<Self>) {
        let Some(mut mode) = self.copy_mode else { return };
        let Some(hit) =
            self.search.as_ref().and_then(|s| s.current.and_then(|c| s.matches.get(c))).copied()
        else {
            return;
        };
        mode.go_to((hit.line, hit.col));
        self.copy_mode = Some(mode);
        self.follow_copy_cursor(0, cx);
    }

    /// Copy mode off: the selection goes, and the view follows the output again, where typing
    /// is.
    pub(super) fn leave_copy_mode(&mut self, cx: &mut Context<Self>) {
        if self.copy_mode.take().is_none() {
            return;
        }
        self.selection = None;
        self.state.scroll_to_bottom();
        cx.notify();
    }

    /// After a key: the selection as copy mode has it, the viewport scrolled `scroll` lines
    /// with the cursor (a page), then on as little as brings the cursor into sight.
    fn follow_copy_cursor(&mut self, scroll: i64, cx: &mut Context<Self>) {
        let Some(mode) = self.copy_mode else { return };
        self.selection = mode.selection(&self.state);
        let rows = u64::from(self.state.size().rows.max(1));
        let mut effects = if scroll == 0 { Vec::new() } else { self.state.scroll(scroll) };
        let top = self.state.index_at_row(0);
        let bottom = top.offset(rows.saturating_sub(1));
        let line = mode.cursor().0;
        let into_sight = if line < top {
            i64::try_from(top.0.saturating_sub(line.0)).unwrap_or(i64::MAX)
        } else if line > bottom {
            i64::try_from(line.0.saturating_sub(bottom.0)).unwrap_or(i64::MAX).saturating_neg()
        } else {
            0
        };
        if into_sight != 0 {
            effects.extend(self.state.scroll(into_sight));
        }
        for effect in effects {
            if let Effect::Request(req) = effect {
                self.send(req, cx);
            }
        }
        cx.notify();
    }

    /// The foot of the grid in copy mode: it says so, and "Done" leaves it, as Esc does.
    pub(super) fn render_copy_mode(&self, cx: &Context<Self>) -> Option<gpui::Div> {
        let pill = FootPill {
            id: "copy-mode",
            icon: crate::icons::IconName::TextCursorInput,
            words: SharedString::new_static(COPY_MODE),
            act: COPY_MODE_DONE,
        };
        self.render_foot_pill(pill, cx, |this, _window, cx| this.leave_copy_mode(cx))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui::{Entity, TestAppContext, VisualTestContext, px};
    use slopty_core::SessionId;
    use slopty_grid::{Cursor, Line, LineIndex, RowUpdate, Style, TermModes};
    use slopty_proto::ClientMsg;
    use slopty_proto::terminal::{Frame, TermEvent, TermRequest, TermSize};
    use slopty_theme::Theme;
    use tokio::sync::mpsc;

    use super::*;

    /// A focused 10 × 3 terminal with the Terminal bindings, holding `history` above a screen of
    /// `screen` with the shell's cursor at `cursor`.
    fn terminal<'a>(
        cx: &'a mut TestAppContext,
        history: &[&str],
        screen: [&str; 3],
        cursor: (u16, u16),
    ) -> (Entity<TerminalView>, mpsc::Receiver<ClientMsg>, &'a mut VisualTestContext) {
        let line = |text: &&str| Line::from_text(text, 10, Style::DEFAULT);
        let size = TermSize { cols: 10, rows: 3, ..TermSize::default() };
        let (history, screen) = (history.iter().map(line).collect(), screen.iter().map(line));
        open(cx, size, history, screen.collect(), cursor)
    }

    /// A focused terminal of `size` with the Terminal bindings: `history` held above `screen`.
    fn open(
        cx: &mut TestAppContext,
        size: TermSize,
        history: Vec<Line>,
        screen: Vec<Line>,
        cursor: (u16, u16),
    ) -> (Entity<TerminalView>, mpsc::Receiver<ClientMsg>, &mut VisualTestContext) {
        let (tx, rx) = mpsc::channel(4096);
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys(super::super::key_bindings());
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let view = TerminalView::new(SessionId::new(), size, tx, Theme::default(), cx);
            window.focus(&view.focus, cx);
            view
        });
        let (width, height) = (f32::from(size.cols) * 40.0, f32::from(size.rows) * 100.0);
        cx.simulate_resize(gpui::size(px(width), px(height)));
        let held = history.len() as u64;
        view.update(cx, |view, cx| {
            view.apply(
                TermEvent::Frame(Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols: size.cols,
                    rows: size.rows,
                    cursor: Cursor { row: cursor.0, col: cursor.1, ..Cursor::default() },
                    modes: TermModes::empty(),
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(held),
                    total_lines: held.saturating_add(u64::from(size.rows)),
                    input_ack: 0,
                    above: None,
                    blocks: None,
                    images: Vec::new(),
                    updates: screen
                        .into_iter()
                        .enumerate()
                        .map(|(row, line)| RowUpdate {
                            row: u16::try_from(row).unwrap(),
                            line: line.into(),
                        })
                        .collect(),
                }),
                cx,
            );
            view.apply(TermEvent::Lines { start: LineIndex(0), lines: history }, cx);
        });
        cx.run_until_parked();
        (view, rx, cx)
    }

    /// What went to the worker since the last call that a program would read as input.
    fn typed(rx: &mut mpsc::Receiver<ClientMsg>) -> usize {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|msg| {
                matches!(
                    msg,
                    ClientMsg::Term {
                        req: TermRequest::Key(_) | TermRequest::Raw(_) | TermRequest::Paste { .. },
                        ..
                    }
                )
            })
            .count()
    }

    fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
    }

    /// Copy mode starts at the shell's cursor; its keys move a cursor of its own, select a
    /// word and copy it, and leave. The program hears none of them, typed or committed by an
    /// input method.
    #[gpui::test]
    fn keys_select_and_copy_without_reaching_the_program(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx, &[], ["$ ls", "src target", "$ "], (2, 2));
        typed(&mut rx);
        cx.dispatch_action(CopyMode);
        assert_eq!(view.read_with(cx, |v, _| v.copy_cursor()), Some((LineIndex(2), 2)));
        cx.simulate_keystrokes("k k v e");
        assert_eq!(
            view.read_with(cx, |v, _| v.copy_cursor()),
            Some((LineIndex(0), 3)),
            "up twice onto `ls` of `$ ls`, then to the word's end"
        );
        cx.simulate_keystrokes("x");
        view.update_in(cx, |v, window, cx| {
            gpui::EntityInputHandler::replace_text_in_range(v, None, "x", window, cx);
        });
        cx.simulate_keystrokes("y");
        assert_eq!(clipboard(cx).as_deref(), Some("ls"));
        assert!(!view.read_with(cx, |v, _| v.in_copy_mode()), "y leaves");
        assert_eq!(view.read_with(cx, |v, _| v.selection()), None);
        assert_eq!(typed(&mut rx), 0, "no key of copy mode reached the program");

        cx.simulate_keystrokes("a");
        assert_eq!(typed(&mut rx), 1, "out of it, keys are the program's again");
    }

    /// Esc drops a selection first and leaves with none; `V` takes whole lines and ⌘C copies
    /// them and leaves.
    #[gpui::test]
    fn escape_drops_the_selection_then_leaves(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx, &[], ["one", "two", "$ "], (2, 2));
        cx.dispatch_action(CopyMode);
        cx.simulate_keystrokes("v k");
        assert!(view.read_with(cx, |v, _| v.selection().is_some()));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |v, _| v.in_copy_mode() && v.selection().is_none()));
        cx.simulate_keystrokes("escape");
        assert!(!view.read_with(cx, |v, _| v.in_copy_mode()));

        cx.dispatch_action(CopyMode);
        cx.simulate_keystrokes("k shift-v k cmd-c");
        assert_eq!(clipboard(cx).as_deref(), Some("one\ntwo"));
        assert!(!view.read_with(cx, |v, _| v.in_copy_mode()), "⌘C leaves too");
    }

    /// The viewport follows the cursor up into the history and back, a line at a time at its
    /// edge; `g` reaches the oldest line; leaving goes back to the live output.
    #[gpui::test]
    fn the_viewport_follows_the_cursor(cx: &mut TestAppContext) {
        let history = ["h0", "h1", "h2", "h3", "h4", "h5"];
        let (view, _rx, cx) = terminal(cx, &history, ["s0", "s1", "$ "], (2, 2));
        let top = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.state.index_at_row(0));
        cx.dispatch_action(CopyMode);
        cx.simulate_keystrokes("k k");
        assert_eq!(top(cx), LineIndex(6), "still in sight");
        cx.simulate_keystrokes("k");
        assert_eq!(top(cx), LineIndex(5), "a line up at the top edge");
        cx.simulate_keystrokes("g");
        assert_eq!(top(cx), LineIndex(0));
        cx.simulate_keystrokes("shift-g");
        assert_eq!(top(cx), LineIndex(6));
        cx.simulate_keystrokes("ctrl-b");
        assert_eq!(view.read_with(cx, |v, _| v.copy_cursor()), Some((LineIndex(6), 0)));
        cx.simulate_keystrokes("q");
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 0, "back to the output");
    }

    /// A selection the pointer made is taken over: the cursor is its moving end, and the keys
    /// grow it from there.
    #[gpui::test]
    fn copy_mode_takes_over_a_pointer_selection(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx, &[], ["abc def", "", "$ "], (2, 2));
        view.update(cx, |v, _| {
            v.selection =
                Some(crate::terminal::Selection::run((LineIndex(0), 0), (LineIndex(0), 2)));
        });
        cx.dispatch_action(CopyMode);
        cx.simulate_keystrokes("w e y");
        assert_eq!(clipboard(cx).as_deref(), Some("abc def"));
    }

    /// With a program asking for the mouse, a click in copy mode is still copy mode's: it puts
    /// the cursor on the cell, and the program hears nothing of it.
    #[gpui::test]
    fn a_click_moves_the_copy_cursor_and_the_program_hears_nothing(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx, &[], ["abc", "", "$ "], (2, 2));
        view.update(cx, |v, cx| {
            v.apply(
                TermEvent::Frame(Frame {
                    seq: 2,
                    full: false,
                    epoch: 0,
                    cols: 10,
                    rows: 3,
                    cursor: Cursor { row: 2, col: 2, ..Cursor::default() },
                    modes: TermModes::MOUSE_TRACKING | TermModes::MOUSE_MOTION,
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    above: None,
                    blocks: None,
                    images: Vec::new(),
                    updates: Vec::new(),
                }),
                cx,
            );
        });
        cx.dispatch_action(CopyMode);
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        let m = view.read_with(cx, |v, _| v.metrics.unwrap());
        let cell = gpui::point(m.origin.x + m.cell_width * 1.5, m.origin.y + m.line_height * 0.5);
        cx.simulate_mouse_move(cell, None, gpui::Modifiers::default());
        cx.simulate_click(cell, gpui::Modifiers::default());
        assert_eq!(view.read_with(cx, |v, _| v.copy_cursor()), Some((LineIndex(0), 1)));
        let mouse = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|msg| matches!(msg, ClientMsg::Term { req: TermRequest::Mouse(_), .. }))
            .count();
        assert_eq!(mouse, 0, "the program heard no press, release or move");
    }

    /// The foot of the grid says copy mode is on, and its "Done" leaves it.
    #[gpui::test]
    fn the_foot_says_copy_mode_and_done_leaves(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx, &[], ["", "", "$ "], (2, 2));
        cx.dispatch_action(CopyMode);
        cx.run_until_parked();
        let pill = cx.debug_bounds("copy-mode").expect("the pill is drawn");
        cx.simulate_click(pill.center(), gpui::Modifiers::default());
        assert!(!view.read_with(cx, |v, _| v.in_copy_mode()));
        assert!(cx.debug_bounds("copy-mode").is_none());
    }

    /// What a key in copy mode costs on the UI thread, from the key to the view's next frame
    /// asked for, on a 200 × 60 terminal holding 10 000 lines of `ls -l`. Run by hand (it
    /// prints, it does not judge); `docs/MEASUREMENTS.md`.
    #[gpui::test]
    #[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
    fn measure_copy_mode_keys(cx: &mut TestAppContext) {
        const HELD: usize = 10_000;
        const ROUNDS: usize = 2_000;
        let size = TermSize { cols: 200, rows: 60, ..TermSize::default() };
        let ls = |i: usize| {
            let text = format!("-rw-r--r--  1 me  staff  {i:>8} Oct  5 12:00 file-{i}.rs");
            Line::from_text(&text, size.cols, Style::DEFAULT)
        };
        let history = (0..HELD).map(ls).collect();
        let screen = (HELD..HELD + 60).map(ls).collect();
        let (view, _rx, cx) = open(cx, size, history, screen, (59, 0));
        cx.dispatch_action(CopyMode);
        let time = |cx: &mut VisualTestContext, keys: &[&str]| {
            let strokes: Vec<Keystroke> =
                keys.iter().map(|k| Keystroke::parse(k).unwrap().with_simulated_ime()).collect();
            let mut took: Vec<Duration> = strokes
                .iter()
                .cycle()
                .take(ROUNDS)
                .map(|stroke| {
                    view.update(cx, |v, cx| {
                        let start = Instant::now();
                        v.copy_key(stroke, None, cx);
                        start.elapsed()
                    })
                })
                .collect();
            took.sort_unstable();
            // Per mille: 500 the median, 990 the 99th percentile, 1000 the slowest.
            let at = |per_mille: usize| {
                let i = took.len().saturating_mul(per_mille) / 1000;
                took[i.min(took.len().saturating_sub(1))].as_secs_f64() * 1e6
            };
            (at(500), at(990), at(1000))
        };
        let cases: [(&str, &[&str]); 9] = [
            ("j k: a line, in sight", &["k", "j"]),
            ("k at the top edge: a line and a scroll", &["k"]),
            ("w b: a word", &["w", "w", "b", "b"]),
            ("e: a word's end", &["e"]),
            ("$ 0: a row's ends", &["$", "0"]),
            ("ctrl-u ctrl-d: half pages", &["ctrl-u", "ctrl-u", "ctrl-d", "ctrl-d"]),
            ("g G: 10 060 lines and back", &["g", "shift-g"]),
            ("V then k: whole lines growing", &["k"]),
            ("v then w: a run growing", &["w"]),
        ];
        for (name, keys) in cases {
            match name {
                "k at the top edge: a line and a scroll" => {
                    view.update(cx, |v, cx| {
                        v.copy_command(Command::Move(copy_mode::Motion::ViewTop), None, cx);
                    });
                }
                "V then k: whole lines growing" => {
                    view.update(cx, |v, cx| {
                        v.copy_command(Command::Move(copy_mode::Motion::Newest), None, cx);
                    });
                    view.update(cx, |v, cx| {
                        v.copy_command(Command::Select(copy_mode::Kind::Lines), None, cx);
                    });
                }
                "v then w: a run growing" => {
                    view.update(cx, |v, cx| {
                        v.copy_command(Command::Move(copy_mode::Motion::Oldest), None, cx);
                    });
                    view.update(cx, |v, cx| {
                        v.copy_command(Command::Select(copy_mode::Kind::Run), None, cx);
                    });
                }
                _ => {}
            }
            let (p50, p99, max) = time(cx, keys);
            println!("MEASURE copy mode {name}: p50 {p50:.2} µs, p99 {p99:.2} µs, max {max:.2} µs");
        }
    }

    /// The worst a word motion meets: `b` and `w` back and forth across 10 000 blank lines.
    /// Run by hand, as above.
    #[gpui::test]
    #[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
    fn measure_a_word_motion_over_blank_history(cx: &mut TestAppContext) {
        const BLANK: usize = 10_000;
        let size = TermSize { cols: 200, rows: 60, ..TermSize::default() };
        let word = Line::from_text("word", size.cols, Style::DEFAULT);
        let blank = Line::from_text("", size.cols, Style::DEFAULT);
        let mut history = vec![word.clone()];
        history.extend(std::iter::repeat_n(blank.clone(), BLANK));
        let mut screen = vec![blank; 59];
        screen.push(word);
        let (view, _rx, cx) = open(cx, size, history, screen, (59, 0));
        cx.dispatch_action(CopyMode);
        let mut took = Vec::new();
        for motion in [copy_mode::Motion::WordBack, copy_mode::Motion::WordNext].repeat(10) {
            took.push(view.update(cx, |v, cx| {
                let start = Instant::now();
                v.copy_command(Command::Move(motion), None, cx);
                start.elapsed()
            }));
            let at = view.read_with(cx, |v, _| v.copy_cursor()).unwrap();
            assert!(at == (LineIndex(0), 0) || at.0 == LineIndex(10_060), "crossed: {at:?}");
        }
        took.sort_unstable();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let (median, max) = (ms(took[took.len() / 2]), ms(took[took.len().saturating_sub(1)]));
        println!("MEASURE copy mode w b over {BLANK} blank lines: {median:.2} ms, max {max:.2} ms");
    }
}
