//! `TerminalView`: one attached session on screen.

use std::time::{Duration, Instant};

use gpui::{
    Autocapitalize, Bounds, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, Keystroke, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, Styled as _, TextInputAction, TextInputConfiguration, UTF16Selection, Window,
    div, point, size,
};
use slopty_client::{Effect, TermState};
use slopty_core::SessionId;
use slopty_grid::{Cursor, LineIndex, TermModes};
use slopty_predict::{Policy, Prediction, Predictor};
use slopty_proto::ClientMsg;
use slopty_proto::input::{MouseAction, MouseButton as ProtoButton, MouseEvent};
use slopty_proto::terminal::{TermEvent, TermRequest, TermSize};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use crate::keys;
use crate::terminal::element::{CellMetrics, TerminalElement};

mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        terminal,
        [
            /// Copy the selection.
            Copy,
            /// Paste the clipboard into the session.
            Paste,
        ]
    );
}
pub use actions::{Copy, Paste};

/// Key bindings for the terminal context.
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    const CTX: Option<&str> = Some("Terminal");
    vec![KeyBinding::new("cmd-c", Copy, CTX), KeyBinding::new("cmd-v", Paste, CTX)]
}

/// A drag selection between two cells, in absolute line indices so it survives scrolling.
/// Both ends are inclusive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    /// Where the drag started.
    pub anchor: (LineIndex, u16),
    /// Where the pointer is.
    pub head: (LineIndex, u16),
}

impl Selection {
    /// `(start, end)` in reading order.
    #[must_use]
    pub fn ordered(self) -> ((LineIndex, u16), (LineIndex, u16)) {
        if self.head < self.anchor { (self.head, self.anchor) } else { (self.anchor, self.head) }
    }

    /// The selected columns on line `index` as `start..end`, if the line is inside the
    /// selection; a line in the middle is selected edge to edge.
    #[must_use]
    pub fn columns(self, index: LineIndex, cols: u16) -> Option<std::ops::Range<u16>> {
        let (start, end) = self.ordered();
        if index < start.0 || index > end.0 {
            return None;
        }
        let from = if index == start.0 { start.1 } else { 0 };
        let to = if index == end.0 { end.1.saturating_add(1).min(cols) } else { cols };
        (from < to).then_some(from..to)
    }
}

/// Things the surrounding UI may want to react to.
#[derive(Clone, Debug)]
pub enum TerminalViewEvent {
    /// Title changed.
    Title(String),
    /// Bell.
    Bell,
    /// The child exited.
    Exited(i32),
}

/// One session's view.
pub struct TerminalView {
    session: SessionId,
    state: TermState,
    out: mpsc::Sender<ClientMsg>,
    focus: FocusHandle,
    theme: Theme,
    key_seq: u64,
    metrics: Option<CellMetrics>,
    pending_size: Option<TermSize>,
    font_family: Option<String>,
    zoom: f32,
    predictor: Predictor,
    /// Text an input method is composing at the cursor (Telex, kana, …), not yet sent.
    marked: Option<String>,
    /// The next key (or typed character) gets Control: the phone key bar's ⌃ toggle.
    sticky_control: bool,
    /// Text selected with the mouse.
    selection: Option<Selection>,
    /// The left button is down and moving it extends the selection.
    selecting: bool,
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView").field("session", &self.session).finish_non_exhaustive()
    }
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

/// `SLOPTY_PREDICT=never|adaptive|always` overrides the local-echo policy (testing on fast links).
fn policy_from_env() -> Policy {
    match std::env::var("SLOPTY_PREDICT").as_deref() {
        Ok("never") => Policy::Never,
        Ok("always") => Policy::Always,
        _ => Policy::Adaptive,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    /// A view for `session`; `out` is the link's sender.
    pub fn new(
        session: SessionId,
        size: TermSize,
        out: mpsc::Sender<ClientMsg>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            session,
            state: TermState::new(size),
            out,
            focus: cx.focus_handle(),
            theme,
            key_seq: 0,
            metrics: None,
            pending_size: None,
            font_family: None,
            zoom: 1.0,
            predictor: Predictor::new(policy_from_env()),
            marked: None,
            sticky_control: false,
            selection: None,
            selecting: false,
        }
    }

    /// The mouse selection, if any.
    #[must_use]
    pub const fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// The selected text: trailing blanks trimmed per line, lines joined with newlines. Lines
    /// not in the scrollback cache come out empty.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let (start, end) = selection.ordered();
        let cols = self.state.size().cols;
        let mut out = String::new();
        let mut index = start.0;
        loop {
            if index != start.0 {
                out.push('\n');
            }
            if let Some(range) = selection.columns(index, cols)
                && let Some(line) = self.state.line(index)
            {
                let mut text = String::new();
                for cell in line.cells.iter().skip(usize::from(range.start)).take(range.len()) {
                    if cell.width.draws_text() {
                        text.push_str(if cell.text.is_empty() { " " } else { cell.text.as_str() });
                    }
                }
                out.push_str(text.trim_end());
            }
            if index >= end.0 {
                break;
            }
            index = index.next();
        }
        Some(out)
    }

    /// ⌘C: the selection to the clipboard (nothing selected: nothing happens).
    pub fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text().filter(|t| !t.is_empty()) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    /// ⌘V: the clipboard into the session (the host brackets it when the program asked).
    pub fn paste_clipboard(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        self.selection = None;
        self.state.scroll_to_bottom();
        self.send(TermRequest::Paste(text));
        cx.notify();
    }

    /// Session id.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// The state (for the element).
    #[must_use]
    pub const fn state(&self) -> &TermState {
        &self.state
    }

    /// The theme (for the element).
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The monospace family the element resolved (first installed from the theme's list).
    #[must_use]
    pub fn font_family(&self) -> Option<&str> {
        self.font_family.as_deref()
    }

    /// Record the resolved family.
    pub fn set_font_family(&mut self, family: String) {
        self.font_family = Some(family);
    }

    /// Paint scale (set by the canvas before each frame).
    pub const fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom;
    }

    /// Link RTT, for the prediction policy.
    pub const fn set_rtt(&mut self, rtt: Option<Duration>) {
        self.predictor.set_rtt(rtt);
    }

    /// Predicted cells to overlay and the cursor to draw, when prediction is showing.
    #[must_use]
    pub fn predictions(&self) -> Option<(Vec<Prediction>, Cursor)> {
        if !self.predictor.visible(Instant::now()) || self.state.view_offset() != 0 {
            return None;
        }
        let pending: Vec<Prediction> = self.predictor.pending().iter().cloned().collect();
        Some((pending, self.predictor.cursor(self.state.cursor())))
    }

    /// Lifetime prediction hits and misses.
    #[must_use]
    pub const fn prediction_stats(&self) -> (u64, u64) {
        self.predictor.stats()
    }

    /// Arm or disarm Control for the next key; a soft keyboard has no Control key of its own.
    pub fn set_sticky_control(&mut self, on: bool, cx: &mut Context<Self>) {
        self.sticky_control = on;
        cx.notify();
    }

    /// Whether the next key gets Control.
    #[must_use]
    pub const fn sticky_control(&self) -> bool {
        self.sticky_control
    }

    /// Send a key as if it had been pressed with the terminal focused (key bar buttons).
    pub fn press(&mut self, mut keystroke: Keystroke, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.sticky_control) {
            keystroke.modifiers.control = true;
        }
        self.key_seq = self.key_seq.wrapping_add(1);
        let key = keys::key_event(self.key_seq, &keystroke, false);
        tracing::trace!(session = %self.session, ?key, "key");
        if self.state.view_offset() != 0 {
            self.state.scroll_to_bottom();
        }
        let _guess = self.predictor.on_key(
            &key,
            self.state.cursor(),
            self.state.size().cols,
            self.state.modes(),
            Instant::now(),
        );
        self.send(TermRequest::Key(key));
        cx.notify();
    }

    /// Composition in progress, drawn at the cursor by the element.
    #[must_use]
    pub fn marked(&self) -> Option<&str> {
        self.marked.as_deref()
    }

    /// Cell metrics measured by the element on its last layout.
    #[must_use]
    pub const fn metrics(&self) -> Option<CellMetrics> {
        self.metrics
    }

    /// Feed an event from the link.
    pub fn apply(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        let reconcile = matches!(event, TermEvent::Frame(_));
        let epoch_before = self.state.epoch();
        if let TermEvent::Frame(frame) = &event {
            tracing::trace!(
                session = %self.session,
                seq = frame.seq,
                full = frame.full,
                epoch = frame.epoch,
                rows = frame.updates.len(),
                cursor = ?frame.cursor,
                view_offset = self.state.view_offset(),
                "frame"
            );
        }
        if matches!(event, TermEvent::Resized { .. }) {
            self.predictor.flush();
            self.selection = None;
        }
        let effects = self.state.apply(event);
        if self.state.epoch() != epoch_before {
            // Line numbering changed (reflow, reset, alt screen): the selection means nothing.
            self.selection = None;
        }
        if reconcile {
            let outcome = self.predictor.on_frame(
                self.state.screen(),
                self.state.input_ack(),
                self.state.epoch().unwrap_or(0),
                Instant::now(),
            );
            if outcome.misses > 0 {
                tracing::debug!(session = %self.session, ?outcome, "prediction miss");
            }
        }
        for effect in effects {
            match effect {
                Effect::Request(req) => self.send(req),
                Effect::Title(t) => cx.emit(TerminalViewEvent::Title(t)),
                Effect::Bell => cx.emit(TerminalViewEvent::Bell),
                Effect::Exited(status) => cx.emit(TerminalViewEvent::Exited(status)),
                Effect::ClipboardWrite(text) => {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
                Effect::ClipboardReadRequest => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        self.send(TermRequest::ClipboardRead { text });
                    }
                }
                Effect::Cwd(_) => {}
                Effect::Error(e) => tracing::warn!(session = %self.session, error = %e, "host"),
            }
        }
        cx.notify();
    }

    /// The element measured the grid: `cols × rows` fit, with these metrics.
    pub fn fitted(&mut self, size: TermSize, metrics: CellMetrics, cx: &mut Context<Self>) {
        self.metrics = Some(metrics);
        if self.state.size() == size || self.pending_size == Some(size) {
            return;
        }
        self.pending_size = Some(size);
        for effect in self.state.resize(size) {
            if let Effect::Request(req) = effect {
                self.send(req);
            }
        }
        cx.notify();
    }

    /// Whether this client's size is the one the PTY follows.
    #[must_use]
    pub const fn driving(&self) -> bool {
        self.state.driving()
    }

    /// Ask the host to make this client the driver: the PTY takes our size from now on.
    pub fn drive(&self) {
        self.send(TermRequest::Drive { drive: true });
    }

    fn send(&self, req: TermRequest) {
        let msg = ClientMsg::Term { session: self.session, req };
        if let Err(e) = self.out.try_send(msg) {
            tracing::warn!(session = %self.session, error = %e, "outbound queue");
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Cmd shortcuts belong to the app.
        if event.keystroke.modifiers.platform {
            return;
        }
        self.selection = None;
        if event.is_held {
            self.key_seq = self.key_seq.wrapping_add(1);
            let key = keys::key_event(self.key_seq, &event.keystroke, true);
            self.send(TermRequest::Key(key));
        } else {
            self.press(event.keystroke.clone(), cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => {
                let line_height = self.metrics.map_or(20.0, |m| f32::from(m.line_height));
                f32::from(p.y) / line_height
            }
        };
        // Wheel up (positive y in GPUI) scrolls into history.
        #[expect(clippy::cast_possible_truncation, reason = "whole lines")]
        let delta = lines.round() as i64;
        if delta == 0 {
            return;
        }
        for effect in self.state.scroll(delta) {
            if let Effect::Request(req) = effect {
                self.send(req);
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let Some((col, row)) = self.metrics.and_then(|m| m.cell_at(event.position)) else {
            return;
        };
        // Left button selects unless the program asked for the mouse (⇧ overrides, as in
        // every terminal); everything else is reported to the program.
        let program_wants_mouse = self.state.modes().contains(TermModes::MOUSE_TRACKING);
        if event.button == MouseButton::Left && (!program_wants_mouse || event.modifiers.shift) {
            let at = (self.state.index_at_row(row), col);
            self.selection = Some(Selection { anchor: at, head: at });
            self.selecting = true;
            cx.notify();
            return;
        }
        self.selection = None;
        let button = match event.button {
            MouseButton::Left => ProtoButton::Left,
            MouseButton::Right => ProtoButton::Right,
            MouseButton::Middle => ProtoButton::Middle,
            MouseButton::Navigate(_) => return,
        };
        let (px, py) = self.metrics.map_or((0, 0), |m| m.pixel_at(event.position));
        self.send(TermRequest::Mouse(MouseEvent {
            action: MouseAction::Press,
            button: Some(button),
            mods: keys::mods(event.modifiers),
            col,
            row,
            px,
            py,
        }));
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        let Some((col, row)) = self.metrics.map(|m| m.cell_at_clamped(event.position)) else {
            return;
        };
        let head = (self.state.index_at_row(row), col);
        if let Some(selection) = &mut self.selection
            && selection.head != head
        {
            selection.head = head;
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.selecting) {
            return;
        }
        // A click without a drag selects nothing.
        if self.selection.is_some_and(|s| s.anchor == s.head) {
            self.selection = None;
        }
        cx.notify();
    }
}

/// Text input on top of the key path.
///
/// Keys reach the terminal through [`TerminalView::key_down`]; this handler makes the platform
/// treat a focused terminal as a text field: iOS raises the soft keyboard, and macOS routes
/// printable keys through the active input method, which previews its composition here
/// (`replace_and_mark_text_in_range`, drawn underlined at the cursor) and commits it with
/// `replace_text_in_range`. The terminal has no editable buffer, so the only range that exists
/// is the marked text's.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        if text.is_empty() {
            cx.notify();
            return;
        }
        // ⌃ armed on the key bar: a single typed character becomes a control key.
        let mut chars = text.chars();
        if self.sticky_control
            && let (Some(c), None) = (chars.next(), chars.next())
            && c.is_ascii()
        {
            let keystroke = Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: c.to_ascii_lowercase().to_string(),
                key_char: None,
            };
            self.press(keystroke, cx);
            return;
        }
        if self.state.view_offset() != 0 {
            self.state.scroll_to_bottom();
        }
        self.send(TermRequest::Raw(text.as_bytes().to_vec()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The cursor cell: where an input method should hang its candidate window.
        let m = self.metrics?;
        let cursor = self.state.cursor();
        let origin = point(
            m.origin.x + m.cell_width * f32::from(cursor.col),
            m.origin.y + m.line_height * f32::from(cursor.row),
        );
        Some(Bounds::new(origin, size(m.cell_width, m.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn text_input_configuration(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> TextInputConfiguration {
        TextInputConfiguration {
            autocorrect: false,
            autocapitalize: Autocapitalize::None,
            suggestions: false,
            input_action: TextInputAction::Enter,
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(TerminalElement::new(cx.entity(), focused).zoom(self.zoom))
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use slopty_grid::{Line, RowUpdate, Style, TermModes};
    use slopty_proto::terminal::{Frame, TermRequest};

    use super::*;

    #[test]
    fn selection_columns_cover_edges_and_middle_lines() {
        let s = Selection { anchor: (LineIndex(7), 5), head: (LineIndex(5), 2) };
        assert_eq!(s.columns(LineIndex(4), 10), None);
        assert_eq!(s.columns(LineIndex(5), 10), Some(2..10));
        assert_eq!(s.columns(LineIndex(6), 10), Some(0..10));
        assert_eq!(s.columns(LineIndex(7), 10), Some(0..6));
        assert_eq!(s.columns(LineIndex(8), 10), None);
        let one = Selection { anchor: (LineIndex(1), 3), head: (LineIndex(1), 3) };
        assert_eq!(one.columns(LineIndex(1), 10), Some(3..4));
    }

    /// A drag across three rows copies the cells between the ends, trailing blanks trimmed.
    #[gpui::test]
    fn selected_text_spans_rows_and_trims(cx: &mut TestAppContext) {
        let (tx, _rx) = mpsc::channel(8);
        let (view, cx) = cx.add_window_view(|_window, cx| {
            let size = TermSize { cols: 10, rows: 3, ..TermSize::default() };
            TerminalView::new(SessionId::new(), size, tx, Theme::default(), cx)
        });
        view.update_in(cx, |view, _window, cx| {
            let rows = ["hello wor", "second", "third row"];
            view.apply(
                TermEvent::Frame(Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols: 10,
                    rows: 3,
                    cursor: Cursor::default(),
                    modes: TermModes::empty(),
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(100),
                    total_lines: 103,
                    input_ack: 0,
                    updates: rows
                        .iter()
                        .enumerate()
                        .map(|(row, text)| RowUpdate {
                            row: u16::try_from(row).unwrap(),
                            line: Line::from_text(text, 10, Style::DEFAULT),
                        })
                        .collect(),
                }),
                cx,
            );
            assert_eq!(view.selected_text(), None);
            view.selection =
                Some(Selection { anchor: (LineIndex(100), 6), head: (LineIndex(102), 4) });
            assert_eq!(view.selected_text().as_deref(), Some("wor\nsecond\nthird"));
            // Backwards drags read the same.
            view.selection =
                Some(Selection { anchor: (LineIndex(102), 4), head: (LineIndex(100), 6) });
            assert_eq!(view.selected_text().as_deref(), Some("wor\nsecond\nthird"));
        });
    }

    /// An input method previews its composition at the cursor and nothing reaches the host
    /// until it commits; the commit goes out as raw bytes and clears the preview.
    #[gpui::test]
    fn composition_is_previewed_then_committed_as_raw_bytes(cx: &mut TestAppContext) {
        let (tx, mut rx) = mpsc::channel(8);
        let (view, cx) = cx.add_window_view(|_window, cx| {
            TerminalView::new(SessionId::new(), TermSize::default(), tx, Theme::default(), cx)
        });
        // Construction attaches; only raw input matters here.
        let is_raw =
            |msg: &ClientMsg| matches!(msg, ClientMsg::Term { req: TermRequest::Raw(_), .. });
        while let Ok(msg) = rx.try_recv() {
            assert!(!is_raw(&msg), "no input before typing");
        }
        view.update_in(cx, |view, window, cx| {
            view.replace_and_mark_text_in_range(None, "ti\u{1ebf}", None, window, cx);
            assert_eq!(view.marked(), Some("ti\u{1ebf}"));
            assert_eq!(view.marked_text_range(window, cx), Some(0..3));
            assert!(rx.try_recv().is_err(), "nothing sent while composing");

            view.replace_and_mark_text_in_range(None, "", None, window, cx);
            assert_eq!(view.marked(), None, "empty marked text ends the preview");

            view.replace_and_mark_text_in_range(None, "vi\u{1ec7}", None, window, cx);
            view.replace_text_in_range(None, "vi\u{1ec7}t", window, cx);
            assert_eq!(view.marked(), None, "commit clears the preview");
        });
        match rx.try_recv() {
            Ok(ClientMsg::Term { req: TermRequest::Raw(bytes), .. }) => {
                assert_eq!(bytes, "vi\u{1ec7}t".as_bytes());
            }
            other => panic!("expected the committed text as raw bytes, got {other:?}"),
        }
        view.update_in(cx, |view, window, cx| {
            view.replace_and_mark_text_in_range(None, "a", None, window, cx);
            view.unmark_text(window, cx);
            assert_eq!(view.marked(), None);
        });
    }

    /// The key bar's ⌃ arms Control for exactly one key, whether it comes from the bar
    /// (`press`) or from the soft keyboard as typed text.
    #[gpui::test]
    fn sticky_control_applies_to_the_next_key_only(cx: &mut TestAppContext) {
        let (tx, mut rx) = mpsc::channel(8);
        let (view, cx) = cx.add_window_view(|_window, cx| {
            TerminalView::new(SessionId::new(), TermSize::default(), tx, Theme::default(), cx)
        });
        while rx.try_recv().is_ok() {}
        let keys = |rx: &mut mpsc::Receiver<ClientMsg>| {
            let mut out = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                if let ClientMsg::Term { req: TermRequest::Key(key), .. } = msg {
                    out.push(key);
                }
            }
            out
        };
        view.update_in(cx, |view, window, cx| {
            view.set_sticky_control(true, cx);
            assert!(view.sticky_control());
            view.replace_text_in_range(None, "c", window, cx);
            assert!(!view.sticky_control(), "consumed by the typed character");
            view.replace_text_in_range(None, "d", window, cx);
        });
        let sent = keys(&mut rx);
        assert_eq!(sent.len(), 1, "only the armed character became a key event");
        assert!(sent[0].mods.contains(slopty_proto::input::Mods::CTRL));
        assert_eq!(sent[0].unshifted, Some('c'));

        view.update_in(cx, |view, _window, cx| {
            view.set_sticky_control(true, cx);
            view.press(
                Keystroke {
                    modifiers: gpui::Modifiers::default(),
                    key: "left".into(),
                    key_char: None,
                },
                cx,
            );
            view.press(
                Keystroke {
                    modifiers: gpui::Modifiers::default(),
                    key: "up".into(),
                    key_char: None,
                },
                cx,
            );
        });
        let sent = keys(&mut rx);
        assert_eq!(sent.len(), 2);
        assert!(sent[0].mods.contains(slopty_proto::input::Mods::CTRL));
        assert!(!sent[1].mods.contains(slopty_proto::input::Mods::CTRL));
    }
}
