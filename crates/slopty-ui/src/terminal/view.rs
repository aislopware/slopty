//! `TerminalView`: one attached session on screen.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Autocapitalize, Bounds, Context, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent,
    Keystroke, LongPressEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, TextInputAction, TextInputConfiguration,
    TouchPhase, UTF16Selection, Window, div, point, px, size,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::{Effect, TermState};
use slopty_core::SessionId;
use slopty_grid::{Cursor, LineIndex, TermModes};
use slopty_predict::{Policy, Prediction, Predictor};
use slopty_proto::ClientMsg;
use slopty_proto::input::{MouseAction, MouseButton as ProtoButton, MouseEvent};
use slopty_proto::terminal::{SearchMatch, TermEvent, TermRequest, TermSize};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use crate::colors::{hsla, hsla_alpha};
use crate::keys;
use crate::terminal::element::{CellMetrics, TerminalElement};
use crate::terminal::url;

/// Hits asked for per search; the host counts every hit regardless.
const SEARCH_MAX: u32 = 5_000;
/// While the search bar is open, output refreshes the hits at most this often.
const SEARCH_REFRESH: Duration = Duration::from_millis(300);

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
            /// Open the search bar (or focus it).
            Find,
            /// Go to the next (newer) hit.
            FindNext,
            /// Go to the previous (older) hit.
            FindPrev,
            /// Close the search bar.
            CloseFind,
        ]
    );
}
pub use actions::{CloseFind, Copy, Find, FindNext, FindPrev, Paste};

/// Key bindings for the terminal context.
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    const CTX: Option<&str> = Some("Terminal");
    vec![
        KeyBinding::new("cmd-c", Copy, CTX),
        KeyBinding::new("cmd-v", Paste, CTX),
        KeyBinding::new("cmd-f", Find, CTX),
        KeyBinding::new("cmd-g", FindNext, CTX),
        KeyBinding::new("cmd-shift-g", FindPrev, CTX),
        // Only while the search field itself is focused: Esc in the grid goes to the program.
        KeyBinding::new("escape", CloseFind, Some("TerminalSearch")),
    ]
}

/// The open search bar.
struct Search {
    input: Entity<InputState>,
    /// What the hits are for.
    needle: String,
    total: u32,
    /// Oldest first.
    matches: Vec<SearchMatch>,
    /// Index into `matches` of the hit the user is on.
    current: Option<usize>,
    /// When the needle was last sent (output refreshes are throttled).
    sent: Instant,
    /// The next reply should jump to its newest hit (the needle just changed).
    reveal: bool,
    /// The needle is a regular expression.
    regex: bool,
    /// The host could not compile the regex.
    invalid: Option<String>,
    _subscription: gpui::Subscription,
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
    /// A long press claimed the touch; moving the finger extends the selection.
    touch_selecting: bool,
    /// The search bar, while open.
    search: Option<Search>,
    /// The search mode the next bar opens with (regex or plain).
    search_regex: bool,
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
            touch_selecting: false,
            search: None,
            search_regex: false,
        }
    }

    /// ⌘F: open the search bar, or put the caret back in it with the text selected.
    pub fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("find"));
            let subscription = cx.subscribe(&input, |this, _input, event, cx| match event {
                InputEvent::Change => this.search_changed(cx),
                InputEvent::PressEnter { shift, .. } => {
                    if *shift {
                        this.step_match(-1, cx);
                    } else {
                        this.step_match(1, cx);
                    }
                }
                InputEvent::Focus | InputEvent::Blur => {}
            });
            self.search = Some(Search {
                input,
                needle: String::new(),
                total: 0,
                matches: Vec::new(),
                current: None,
                sent: Instant::now(),
                reveal: false,
                regex: self.search_regex,
                invalid: None,
                _subscription: subscription,
            });
        }
        if let Some(search) = &self.search {
            search.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        cx.notify();
    }

    /// ⌘G / Enter.
    pub fn find_next(&mut self, _: &FindNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_match(1, cx);
    }

    /// ⌘⇧G / ⇧Enter.
    pub fn find_prev(&mut self, _: &FindPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_match(-1, cx);
    }

    /// Esc in the search field: close it and give the keys back to the program.
    pub fn close_find(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Whether the search bar is open.
    #[must_use]
    pub const fn finding(&self) -> bool {
        self.search.is_some()
    }

    /// The hits to paint (oldest first) and which one is current.
    #[must_use]
    pub fn search_highlights(&self) -> Option<(&[SearchMatch], Option<usize>)> {
        self.search.as_ref().map(|s| (s.matches.as_slice(), s.current))
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let needle = search.input.read(cx).value().to_string();
        if needle == search.needle {
            return;
        }
        search.needle = needle;
        self.restart_search(cx);
    }

    /// Drop the hits and ask again (the needle or the mode changed).
    fn restart_search(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        search.matches.clear();
        search.total = 0;
        search.current = None;
        search.invalid = None;
        search.reveal = true;
        self.send_search(cx);
    }

    /// Flip the search between plain text and regex; remembered for the next search bar.
    fn toggle_search_regex(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        search.regex = !search.regex;
        self.search_regex = search.regex;
        self.restart_search(cx);
    }

    /// The host rejected the regex.
    fn search_invalid(&mut self, needle: &str, message: String, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        if needle != search.needle {
            return;
        }
        search.matches.clear();
        search.total = 0;
        search.current = None;
        search.invalid = Some(message);
        cx.notify();
    }

    fn send_search(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        search.sent = Instant::now();
        let needle = search.needle.clone();
        if needle.is_empty() {
            cx.notify();
            return;
        }
        let regex = search.regex;
        self.send(TermRequest::Search { needle, max: SEARCH_MAX, regex });
    }

    /// Move `by` hits (wrapping) and scroll the new one into view.
    fn step_match(&mut self, by: i64, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        if search.matches.is_empty() {
            return;
        }
        let n = search.matches.len();
        let at = search.current.map_or_else(
            || if by > 0 { 0 } else { n.saturating_sub(1) },
            |c| {
                let next = i64::try_from(c).unwrap_or(0).saturating_add(by);
                let n = i64::try_from(n).unwrap_or(1);
                usize::try_from(next.rem_euclid(n)).unwrap_or(0)
            },
        );
        search.current = Some(at);
        self.reveal_current(cx);
    }

    /// Scroll so the current hit sits in the viewport (centred when it was off screen).
    fn reveal_current(&mut self, cx: &mut Context<Self>) {
        let Some(hit) =
            self.search.as_ref().and_then(|s| s.current.and_then(|c| s.matches.get(c))).copied()
        else {
            return;
        };
        let rows = u64::from(self.state.size().rows);
        let top = self.state.index_at_row(0);
        let visible = hit.line >= top && hit.line.0 < top.0.saturating_add(rows);
        if !visible {
            // Offset counts up from the bottom: the first visible line when following output.
            let first_visible = top.0.saturating_add(self.state.view_offset());
            let wanted_top = hit.line.0.saturating_sub(rows / 2);
            let offset = first_visible.saturating_sub(wanted_top);
            for effect in self.state.scroll_to(offset) {
                if let Effect::Request(req) = effect {
                    self.send(req);
                }
            }
        }
        cx.notify();
    }

    fn matches_arrived(
        &mut self,
        needle: &str,
        total: u32,
        matches: Vec<SearchMatch>,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = &mut self.search else { return };
        if needle != search.needle {
            return;
        }
        // Keep the user on the same hit across a refresh when it is still there.
        let on = search.current.and_then(|c| search.matches.get(c)).copied();
        search.invalid = None;
        search.total = total;
        search.matches = matches;
        search.current = on.and_then(|hit| search.matches.iter().position(|m| *m == hit));
        if search.current.is_none() && !search.matches.is_empty() {
            search.current = Some(search.matches.len().saturating_sub(1));
        }
        if std::mem::take(&mut search.reveal) {
            self.reveal_current(cx);
        }
        cx.notify();
    }

    /// The mouse selection, if any.
    #[must_use]
    pub const fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// The word under `col` on line `index` as inclusive columns: the run of non-blank cells
    /// around it, or just the cell when it is blank.
    fn word_at(&self, index: LineIndex, col: u16) -> (u16, u16) {
        let Some(line) = self.state.line(index) else { return (col, col) };
        let blank = |c: u16| {
            line.cells
                .get(usize::from(c))
                .is_none_or(|cell| cell.width.draws_text() && cell.text.as_str().trim().is_empty())
        };
        if blank(col) {
            return (col, col);
        }
        let mut start = col;
        while start > 0 && !blank(start.wrapping_sub(1)) {
            start = start.wrapping_sub(1);
        }
        let mut end = col;
        while !blank(end.wrapping_add(1)) {
            end = end.wrapping_add(1);
        }
        (start, end)
    }

    /// Select `cols` of line `index`: the word at `col` for two clicks, the line for more.
    fn select_by_clicks(&mut self, index: LineIndex, col: u16, clicks: usize) {
        let (start, end) = if clicks == 2 {
            self.word_at(index, col)
        } else {
            (0, self.state.size().cols.saturating_sub(1))
        };
        self.selection = Some(Selection { anchor: (index, start), head: (index, end) });
    }

    /// A touch long press over the terminal. On the phone a plain drag pans the canvas, so
    /// selection follows the platform convention: hold to select the word under the finger,
    /// keep holding and move to extend it. Returns whether the press was claimed (the element
    /// then keeps the gesture away from the canvas).
    pub fn long_press(
        &mut self,
        event: &LongPressEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match event.phase {
            TouchPhase::Started => {
                let Some((col, row)) = self.metrics.and_then(|m| m.cell_at(event.start_position))
                else {
                    return false;
                };
                self.focus.focus(window, cx);
                self.select_by_clicks(self.state.index_at_row(row), col, 2);
                self.touch_selecting = true;
                cx.notify();
                true
            }
            TouchPhase::Moved => {
                if !self.touch_selecting {
                    return false;
                }
                if let Some((col, row)) = self.metrics.map(|m| m.cell_at_clamped(event.position))
                    && let Some(selection) = &mut self.selection
                {
                    let head = (self.state.index_at_row(row), col);
                    if selection.head != head {
                        selection.head = head;
                        cx.notify();
                    }
                }
                true
            }
            TouchPhase::Ended => std::mem::take(&mut self.touch_selecting),
            TouchPhase::Cancelled => {
                if std::mem::take(&mut self.touch_selecting) {
                    self.selection = None;
                    cx.notify();
                }
                false
            }
        }
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

    /// Drop the selection.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.selection.take().is_some() {
            cx.notify();
        }
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

    /// Swap the theme. A changed family list drops the resolved family so the element picks
    /// again; the element re-measures the grid from the new size on its next frame and
    /// `fitted` resizes the session.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        if self.theme.typography.mono_families != theme.typography.mono_families {
            self.font_family = None;
        }
        self.theme = theme;
        cx.notify();
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
            // Line numbering changed (reflow, reset, alt screen): the selection means nothing,
            // and neither do the search hits.
            self.selection = None;
            if let Some(search) = &mut self.search {
                search.matches.clear();
                search.current = None;
            }
        }
        if reconcile
            && let Some(search) = &self.search
            && !search.needle.is_empty()
            && search.sent.elapsed() >= SEARCH_REFRESH
        {
            self.send_search(cx);
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
                Effect::Matches { needle, total, matches } => {
                    self.matches_arrived(&needle, total, matches, cx);
                }
                Effect::SearchInvalid { needle, message } => {
                    self.search_invalid(&needle, message, cx);
                }
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

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Cmd shortcuts belong to the app.
        if event.keystroke.modifiers.platform {
            return;
        }
        // Typing in the search field must never reach the program.
        if self
            .search
            .as_ref()
            .is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window))
        {
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
        // ⌘-click opens the link under the pointer, as in every terminal.
        if event.button == MouseButton::Left && event.modifiers.platform {
            let index = self.state.index_at_row(row);
            if let Some(url) = self.state.line(index).and_then(|line| url::url_at_col(line, col)) {
                tracing::info!(%url, "open link");
                cx.open_url(&url);
            }
            return;
        }
        // Left button selects unless the program asked for the mouse (⇧ overrides, as in
        // every terminal); everything else is reported to the program.
        let program_wants_mouse = self.state.modes().contains(TermModes::MOUSE_TRACKING);
        if event.button == MouseButton::Left && (!program_wants_mouse || event.modifiers.shift) {
            let index = self.state.index_at_row(row);
            if event.click_count >= 2 {
                // Word, then line; the selection stands until the next click.
                self.select_by_clicks(index, col, event.click_count);
                self.selecting = false;
            } else {
                let at = (index, col);
                self.selection = Some(Selection { anchor: at, head: at });
                self.selecting = true;
            }
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

impl TerminalView {
    /// The search bar: field, "n/total", close. Sits over the top-right corner of the grid.
    fn render_search(&self, search: &Search, cx: &Context<Self>) -> impl IntoElement {
        let s = &self.theme.surfaces;
        let count: SharedString = if search.needle.is_empty() {
            SharedString::default()
        } else if search.invalid.is_some() {
            "bad regex".into()
        } else if search.matches.is_empty() {
            "none".into()
        } else {
            let at = search.current.map_or(0, |c| c.saturating_add(1));
            let more = if search.total > SEARCH_MAX { "+" } else { "" };
            format!("{at}/{}{more}", search.total).into()
        };
        div()
            .id("terminal-search")
            .key_context("TerminalSearch")
            .absolute()
            .top(px(6.0))
            // A phone-wide terminal can be wider than the screen; its left edge is the part
            // that is on screen (the "take" pill sits there for the same reason).
            .when(cfg!(target_os = "ios"), |bar| bar.left(px(6.0)))
            .when(!cfg!(target_os = "ios"), |bar| bar.right(px(6.0)))
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.accent))
            .shadow_md()
            .text_size(px(12.0))
            .text_color(hsla(s.text))
            .font_family(self.theme.typography.ui_family.clone())
            .on_action(cx.listener(Self::close_find))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .child(div().w(px(180.0)).child(Input::new(&search.input)))
            .child(
                div()
                    .id("terminal-search-regex")
                    .px(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_color(hsla(if search.regex { s.canvas } else { s.text_muted }))
                    .when(search.regex, |el| el.bg(hsla(s.accent)))
                    .hover(|st| st.bg(hsla_alpha(s.text, 0.1)))
                    .child(".*")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.toggle_search_regex(cx))),
            )
            .child(div().min_w(px(40.0)).text_color(hsla(s.text_muted)).child(count))
            .child(
                div()
                    .id("terminal-search-prev")
                    .px(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_color(hsla(s.text_muted))
                    .hover(|st| st.bg(hsla_alpha(s.text, 0.1)))
                    .child("↑")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_match(-1, cx))),
            )
            .child(
                div()
                    .id("terminal-search-next")
                    .px(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_color(hsla(s.text_muted))
                    .hover(|st| st.bg(hsla_alpha(s.text, 0.1)))
                    .child("↓")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_match(1, cx))),
            )
            .child(
                div()
                    .id("terminal-search-close")
                    .px(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_color(hsla(s.text_muted))
                    .hover(|st| st.bg(hsla_alpha(s.text, 0.1)))
                    .child("✕")
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.close_find(&CloseFind, window, cx);
                    })),
            )
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let search = self.search.as_ref().map(|s| self.render_search(s, cx));
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_prev))
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(TerminalElement::new(cx.entity(), focused).zoom(self.zoom))
            .children(search)
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
            // Two clicks take the word, three the line, a blank cell only itself.
            view.select_by_clicks(LineIndex(100), 7, 2);
            assert_eq!(view.selected_text().as_deref(), Some("wor"));
            view.select_by_clicks(LineIndex(100), 5, 2);
            assert_eq!(
                view.selection.map(Selection::ordered),
                Some(((LineIndex(100), 5), (LineIndex(100), 5)))
            );
            view.select_by_clicks(LineIndex(102), 0, 3);
            assert_eq!(view.selected_text().as_deref(), Some("third row"));
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
