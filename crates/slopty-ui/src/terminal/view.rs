//! `TerminalView`: one attached session on screen.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Autocapitalize, Bounds, Context, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent,
    Keystroke, LongPressEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, TextInputAction, TextInputConfiguration,
    TouchPhase, UTF16Selection, Window, div, point, px, size,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::{Effect, TermState};
use slopty_core::SessionId;
use slopty_grid::{Cursor, LineIndex, TermModes};
use slopty_predict::{Policy, Prediction, Predictor};
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentStatus, BlockReason, TranscriptFollow, TranscriptUpdate};
use slopty_proto::input::{MouseAction, MouseButton as ProtoButton, MouseEvent};
use slopty_proto::terminal::{SearchMatch, TermEvent, TermRequest, TermSize};
use slopty_theme::{Theme, alpha};
use tokio::sync::mpsc;

use crate::colors::{hsla, hsla_alpha};
use crate::keys;
use crate::terminal::conversation::{Attention, Conversation};
use crate::terminal::element::{CellMetrics, TerminalElement};
use crate::terminal::{latency, url};

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
            /// Scroll the previous prompt to the top of the viewport.
            PrevPrompt,
            /// Scroll the next prompt to the top of the viewport.
            NextPrompt,
            /// Copy the output of the last command.
            CopyLastOutput,
            ToggleConversation,
        ]
    );
}
pub use actions::{
    CloseFind, Copy, CopyLastOutput, Find, FindNext, FindPrev, NextPrompt, Paste, PrevPrompt,
    ToggleConversation,
};

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
        KeyBinding::new("cmd-up", PrevPrompt, CTX),
        KeyBinding::new("cmd-down", NextPrompt, CTX),
        KeyBinding::new("cmd-shift-c", CopyLastOutput, CTX),
        KeyBinding::new("cmd-shift-l", ToggleConversation, CTX),
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
    /// The working directory changed (OSC 7), with the repository the host resolved it to.
    /// What arrange-by-repo groups on, so it has to follow a `cd` and not stay at whatever the
    /// session opened in.
    Cwd {
        /// The new directory.
        path: String,
        /// Its repository root, if it is in one.
        repo: Option<String>,
    },
    /// Bell.
    Bell,
    /// The child exited.
    Exited(i32),
    /// The conversation's Allow / Deny row was pressed: the canvas types the answer, as for
    /// the title-bar badge.
    Answered {
        /// Allow (Enter) or deny (Esc).
        allowed: bool,
    },
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
    /// The canvas zoom is in motion this frame (set by the canvas before each frame).
    zooming: bool,
    /// Frames drawn while zooming (tests).
    #[cfg(test)]
    motion_frames: u32,
    predictor: Predictor,
    /// Keystroke → paint, predicted and echoed (see [`latency`]).
    latency: latency::KeyLatency,
    /// Text an input method is composing at the cursor (Telex, kana, …), not yet sent.
    marked: Option<String>,
    /// The next key (or typed character) gets Control: the phone key bar's ⌃ toggle.
    sticky_control: bool,
    /// The next tap opens the link under it, as ⌘-click does: the phone key bar's ⌘ toggle.
    sticky_command: bool,
    /// Text selected with the mouse.
    selection: Option<Selection>,
    /// The left button is down and moving it extends the selection.
    selecting: bool,
    /// The cell under the pointer, for the ⌘-hover link underline.
    hover: Option<(u16, u16)>,
    /// ⌘ is down: links under the pointer show as links.
    cmd_held: bool,
    /// A long press claimed the touch; moving the finger extends the selection.
    touch_selecting: bool,
    /// The search bar, while open.
    search: Option<Search>,
    /// The agent's conversation shown in place of the grid (see [`Conversation`]).
    conversation: Option<Conversation>,
    /// The agent's state in this session, as the host last reported it.
    agent: Option<AgentStatus>,
    /// The conversation's row answered a permission; cleared by the host's next report.
    answered: Option<bool>,
    /// Put the caret in the composer on the next frame (the agent asked something).
    focus_composer: bool,
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
            zooming: false,
            #[cfg(test)]
            motion_frames: 0,
            predictor: Predictor::new(policy_from_env()),
            latency: latency::KeyLatency::default(),
            marked: None,
            sticky_control: false,
            sticky_command: false,
            selection: None,
            selecting: false,
            hover: None,
            cmd_held: false,
            touch_selecting: false,
            search: None,
            conversation: None,
            agent: None,
            answered: None,
            focus_composer: false,
            search_regex: false,
        }
    }

    /// ⌘⇧L or the title-bar pill: show the agent's conversation instead of the grid, or the
    /// grid again. The host is told to start or stop tailing the transcript. The composer
    /// takes the keyboard with the conversation; the grid takes it back.
    pub fn toggle_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let follow = self.conversation.is_none();
        if follow {
            let conversation = Conversation::new(window, cx);
            conversation.focus_composer(window, cx);
            self.conversation = Some(conversation);
        } else {
            self.conversation = None;
            self.focus.focus(window, cx);
        }
        let msg = ClientMsg::Transcript(TranscriptFollow { session: self.session, follow });
        if let Err(e) = self.out.try_send(msg) {
            tracing::warn!(session = %self.session, error = %e, "outbound queue");
        }
        cx.notify();
    }

    /// ↩ in the composer (or its send button): the text goes into the session as a paste,
    /// then Enter; an empty composer sends the bare Enter, which accepts whatever the agent
    /// is offering.
    pub fn submit_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conversation) = &self.conversation else { return };
        let text = conversation.take_composer_text(window, cx);
        if !text.trim().is_empty() {
            self.send(TermRequest::Paste(text));
        }
        let enter = Keystroke {
            modifiers: gpui::Modifiers::default(),
            key: "enter".to_owned(),
            key_char: None,
        };
        self.press(enter, cx);
    }

    /// The conversation's Allow / Deny: remembered until the host reports the agent's next
    /// state, and handed to the canvas, which types the same key as the title-bar badge.
    pub fn answer(&mut self, allowed: bool, cx: &mut Context<Self>) {
        if self.answered.is_some() {
            return;
        }
        self.answered = Some(allowed);
        cx.emit(TerminalViewEvent::Answered { allowed });
        cx.notify();
    }

    /// The host's word on the agent in this session (`None`: no agent). A question or an
    /// elicitation puts the caret in the composer when the conversation is on.
    pub fn set_agent_status(&mut self, status: Option<AgentStatus>, cx: &mut Context<Self>) {
        self.answered = None;
        if self.conversation.is_some()
            && matches!(
                status,
                Some(AgentStatus::Blocked(BlockReason::Question | BlockReason::Elicitation))
            )
        {
            self.focus_composer = true;
        }
        self.agent = status;
        cx.notify();
    }

    /// The agent's state as last reported.
    #[must_use]
    pub const fn agent_status(&self) -> Option<&AgentStatus> {
        self.agent.as_ref()
    }

    /// What the conversation shows above its composer: the pending permission with its
    /// answers, or that the agent waits for a reply.
    #[must_use]
    pub fn attention(&self) -> Option<Attention> {
        match self.agent.as_ref()? {
            AgentStatus::Blocked(BlockReason::Permission { tool }) => {
                Some(Attention::Permission { tool: tool.clone(), answered: self.answered })
            }
            AgentStatus::Blocked(BlockReason::Question | BlockReason::Elicitation) => {
                Some(Attention::Prompt)
            }
            _ => None,
        }
    }

    /// Open or fold an entry's long part (thinking, tool input, the rest of a result).
    pub fn toggle_entry(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(conversation) = &mut self.conversation {
            conversation.toggle(ix);
            cx.notify();
        }
    }

    /// Whether the composer holds the keyboard.
    fn composer_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.conversation.as_ref().is_some_and(|c| c.composer_focus(cx).is_focused(window))
    }

    /// The conversation on show, if any.
    #[must_use]
    pub const fn conversation(&self) -> Option<&Conversation> {
        self.conversation.as_ref()
    }

    /// A slice of the transcript from the host; ignored once the conversation is hidden.
    pub fn transcript_update(&mut self, update: TranscriptUpdate, cx: &mut Context<Self>) {
        if let Some(conversation) = &mut self.conversation {
            conversation.apply(update);
            cx.notify();
        }
    }

    fn toggle_conversation_action(
        &mut self,
        _: &ToggleConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_conversation(window, cx);
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

    /// The link to underline: the run under the pointer while ⌘ is held, as
    /// `(line, first column, one past the last)`.
    #[must_use]
    pub fn link_highlight(&self) -> Option<(LineIndex, u16, u16)> {
        if !self.cmd_held {
            return None;
        }
        let (col, row) = self.hover?;
        let index = self.state.index_at_row(row);
        let line = self.state.line(index)?;
        url::link_at_col(line, col).map(|span| (index, span.start, span.end))
    }

    /// Pointer position and ⌘ state changed; repaint only when the underline moves.
    fn set_pointer(&mut self, hover: Option<(u16, u16)>, cmd: bool, cx: &mut Context<Self>) {
        let before = self.link_highlight();
        self.hover = hover;
        self.cmd_held = cmd;
        if self.link_highlight() != before {
            cx.notify();
        }
    }

    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_pointer(self.hover, event.modifiers.platform, cx);
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

    /// ⌘⇧C: the last command's output (shell integration marks it) to the clipboard.
    pub fn copy_last_output(
        &mut self,
        _: &CopyLastOutput,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = self.state.last_command_output() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    /// ⌘↑: the prompt above the viewport's top row, scrolled to the top.
    pub fn prev_prompt(&mut self, _: &PrevPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        let top = self.state.index_at_row(0);
        if let Some(target) = self.state.prompt_before(top) {
            self.jump_to(target, cx);
        }
    }

    /// ⌘↓: the prompt below the viewport's top row, scrolled to the top; none left means back
    /// to following output.
    pub fn next_prompt(&mut self, _: &NextPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        let top = self.state.index_at_row(0);
        if let Some(target) = self.state.prompt_after(top) {
            self.jump_to(target, cx);
        } else {
            self.state.scroll_to_bottom();
            cx.notify();
        }
    }

    fn jump_to(&mut self, index: LineIndex, cx: &mut Context<Self>) {
        for effect in self.state.scroll_to_line(index) {
            if let Effect::Request(req) = effect {
                self.send(req);
            }
        }
        cx.notify();
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

    /// Whether the zoom is in motion this frame (set by the canvas before each frame): the
    /// grid paints from the raster ladder instead of rasterising every glyph at a new size.
    pub const fn set_zooming(&mut self, on: bool) {
        self.zooming = on;
    }

    /// How many frames this view drew while the zoom was in motion.
    #[cfg(test)]
    pub const fn motion_frames(&self) -> u32 {
        self.motion_frames
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

    /// The element painted a frame whose local-echo overlay showed guesses for the keys in
    /// `shown` (their sequence numbers).
    pub fn painted(&mut self, shown: &[u64]) {
        self.latency.painted(Instant::now(), shown, self.state.input_ack());
    }

    /// Keystroke → paint percentiles (see [`latency`]).
    #[must_use]
    pub fn latency(&self) -> latency::LatencyStats {
        self.latency.stats()
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

    /// Arm or disarm ⌘ for the next tap: it opens the link under the finger, as ⌘-click
    /// does with a mouse (a phone has no ⌘ to hold).
    pub fn set_sticky_command(&mut self, on: bool, cx: &mut Context<Self>) {
        self.sticky_command = on;
        cx.notify();
    }

    /// Whether the next tap opens a link.
    #[must_use]
    pub const fn sticky_command(&self) -> bool {
        self.sticky_command
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
        let now = Instant::now();
        self.latency.pressed(self.key_seq, now);
        let _guess = self.predictor.on_key(
            &key,
            self.state.cursor(),
            self.state.size().cols,
            self.state.modes(),
            now,
        );
        self.send(TermRequest::Key(key));
        cx.notify();
    }

    /// The visible rows as text, top to bottom, trailing spaces trimmed; a row still being
    /// fetched is empty. What a self-test reads instead of pixels.
    #[must_use]
    pub fn rows(&self) -> Vec<String> {
        self.state
            .view()
            .iter()
            .map(|row| row.line.map(|l| l.text().trim_end().to_owned()).unwrap_or_default())
            .collect()
    }

    /// Program title (OSC 0/2), if set.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.state.title()
    }

    /// The cursor row's text, trailing spaces trimmed: what a screen reader reads as the
    /// grid's value (the whole grid would be noise).
    #[must_use]
    pub fn cursor_row_text(&self) -> String {
        let row = usize::from(self.state.cursor().row);
        self.state
            .view()
            .get(row)
            .and_then(|r| r.line.map(|l| l.text().trim_end().to_owned()))
            .unwrap_or_default()
    }

    /// Grid size.
    #[must_use]
    pub const fn size(&self) -> TermSize {
        self.state.size()
    }

    /// Cursor position.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.state.cursor()
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
                Effect::Cwd { path, repo } => cx.emit(TerminalViewEvent::Cwd { path, repo }),
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
            tracing::debug!(session = %self.session, key = %event.keystroke.key, "cmd key passed up");
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
        // Typing in the composer stays there; Esc and Control keys (⌃C above all) keep their
        // terminal meaning, so the agent can be interrupted without leaving the chat.
        if self.composer_focused(window, cx) {
            let k = &event.keystroke;
            if !(k.key == "escape" || k.modifiers.control) {
                if k.key == "enter" && !k.modifiers.shift {
                    // The composer's Enter action submitted and let the action through; the
                    // key must not also type a newline into it.
                    cx.stop_propagation();
                }
                return;
            }
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
        // The conversation's list scrolls itself.
        if self.conversation.is_some() {
            return;
        }
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
        // The conversation's controls (composer, folds, buttons) take their own clicks.
        if self.conversation.is_some() {
            return;
        }
        self.focus.focus(window, cx);
        let Some((col, row)) = self.metrics.and_then(|m| m.cell_at(event.position)) else {
            return;
        };
        // ⌘-click opens the link under the pointer, as in every terminal: the program's OSC 8
        // target when there is one, else the URL in the text. The key bar's ⌘ arms one tap.
        let armed = event.button == MouseButton::Left && std::mem::take(&mut self.sticky_command);
        if armed {
            cx.notify();
        }
        if event.button == MouseButton::Left && (event.modifiers.platform || armed) {
            let index = self.state.index_at_row(row);
            if let Some(span) = self.state.line(index).and_then(|line| url::link_at_col(line, col))
            {
                tracing::info!(url = %span.url, "open link");
                cx.open_url(&span.url);
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
        if self.conversation.is_some() {
            return;
        }
        let hover = self.metrics.and_then(|m| m.cell_at(event.position));
        self.set_pointer(hover, event.modifiers.platform, cx);
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
    fn render_search(
        &self,
        search: &Search,
        focused: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let wash = hsla_alpha(s.text, alpha::HOVER);
        let bare = move |id: &'static str| {
            div()
                .id(id)
                .px(px(spacing.xs))
                .rounded(px(radii.xs))
                .cursor_pointer()
                .text_color(hsla(s.text_muted))
                .hover(move |st| st.bg(wash))
        };
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
            .top(px(spacing.sm))
            // A phone-wide terminal can be wider than the screen; its left edge is the part
            // that is on screen (the "take" pill sits there for the same reason).
            .when(cfg!(target_os = "ios"), |bar| bar.left(px(spacing.sm)))
            .when(!cfg!(target_os = "ios"), |bar| bar.right(px(spacing.sm)))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(radii.sm))
            .bg(hsla(s.panel))
            .border_1()
            // The focus ring: accent while the field has the caret, a hairline otherwise.
            .border_color(hsla(if focused { s.accent } else { s.border }))
            .shadow_sm()
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .font_family(self.theme.typography.ui_family.clone())
            .on_action(cx.listener(Self::close_find))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .role(gpui::accesskit::Role::Group)
            .aria_label("Find")
            .child(div().w(px(180.0)).child(Input::new(&search.input).aria_label("Find")))
            .child(
                bare("terminal-search-regex")
                    .role(gpui::accesskit::Role::Button)
                    .aria_label(if search.regex { "Plain text" } else { "Regular expression" })
                    .when(search.regex, |el| el.bg(hsla(s.accent)).text_color(hsla(s.accent_fg)))
                    .child(".*")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.toggle_search_regex(cx))),
            )
            .child(div().min_w(px(40.0)).text_color(hsla(s.text_secondary)).child(count))
            .child(
                bare("terminal-search-prev")
                    .role(gpui::accesskit::Role::Button)
                    .aria_label("Previous match")
                    .child("↑")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_match(-1, cx))),
            )
            .child(
                bare("terminal-search-next")
                    .role(gpui::accesskit::Role::Button)
                    .aria_label("Next match")
                    .child("↓")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_match(1, cx))),
            )
            .child(
                bare("terminal-search-close")
                    .role(gpui::accesskit::Role::Button)
                    .aria_label("Close find")
                    .child("✕")
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.close_find(&CloseFind, window, cx);
                    })),
            )
            .into_any_element()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let zooming = self.zooming;
        #[cfg(test)]
        if zooming {
            self.motion_frames = self.motion_frames.saturating_add(1);
        }
        if std::mem::take(&mut self.focus_composer) && self.conversation.is_some() {
            // Focus moves after this update, not from inside the render.
            cx.defer_in(window, |this, window, cx| {
                if let Some(conversation) = &this.conversation {
                    conversation.focus_composer(window, cx);
                }
            });
        }
        let attention = self.attention();
        let composer_focused = self.composer_focused(window, cx);
        let conversation = self
            .conversation
            .as_ref()
            .map(|c| c.render(attention.as_ref(), composer_focused, &self.theme, cx));
        let search_focused = self
            .search
            .as_ref()
            .is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window));
        let search = self.search.as_ref().map(|s| self.render_search(s, search_focused, cx));
        div()
            .id("terminal")
            .debug_selector(|| "terminal".to_owned())
            .key_context("Terminal")
            .track_focus(&self.focus)
            .role(gpui::accesskit::Role::Group)
            .relative()
            .size_full()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_prev))
            .on_action(cx.listener(Self::prev_prompt))
            .on_action(cx.listener(Self::next_prompt))
            .on_action(cx.listener(Self::copy_last_output))
            .on_action(cx.listener(Self::toggle_conversation_action))
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .map(|el| {
                if let Some(conversation) = conversation {
                    return el.child(conversation);
                }
                let mut grid =
                    TerminalElement::new(cx.entity(), focused).zoom(self.zoom).zooming(zooming);
                if window.is_a11y_active() {
                    let label = self.title().unwrap_or("shell").to_owned();
                    grid = grid.a11y(label.into(), self.cursor_row_text().into());
                }
                el.child(grid)
            })
            .children(search)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Pixels, TestAppContext, VisualTestContext, px, size};
    use slopty_grid::{Line, RowUpdate, SemanticMark, Style, TermModes};
    use slopty_proto::agent::{Clipped, TranscriptBody, TranscriptEntry};
    use slopty_proto::terminal::{Frame, TermRequest};

    use super::*;
    use crate::terminal::element::separator_color;

    /// A focused terminal in a headless window with the Terminal bindings, drawn once.
    fn terminal(
        cx: &mut TestAppContext,
    ) -> (Entity<TerminalView>, mpsc::Receiver<ClientMsg>, &mut VisualTestContext) {
        let (tx, rx) = mpsc::channel(64);
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys(key_bindings());
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let size = TermSize { cols: 10, rows: 3, ..TermSize::default() };
            let view = TerminalView::new(SessionId::new(), size, tx, Theme::default(), cx);
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(400.0), px(300.0)));
        cx.run_until_parked();
        (view, rx, cx)
    }

    fn user(text: &str) -> TranscriptEntry {
        TranscriptEntry { at: None, body: TranscriptBody::User { text: text.to_owned() } }
    }

    fn assistant(markdown: &str) -> TranscriptEntry {
        TranscriptEntry {
            at: None,
            body: TranscriptBody::Assistant { markdown: markdown.to_owned() },
        }
    }

    fn tool(command: &str) -> TranscriptEntry {
        TranscriptEntry {
            at: None,
            body: TranscriptBody::ToolUse {
                name: "Bash".to_owned(),
                summary: command.to_owned(),
                input: Clipped::whole(format!("{{\n  \"command\": \"{command}\"\n}}")),
            },
        }
    }

    fn marked(text: &str, mark: SemanticMark) -> Line {
        let mut line = Line::from_text(text, 10, Style::DEFAULT);
        line.mark = mark;
        line
    }

    /// Three command blocks: `ls` (a, b), `false` (nothing), `seq 2` (1, 2, blank), then the
    /// newest prompt. Lines 0..=5 are history the host already sent, 6..=8 the screen.
    fn with_command_blocks(view: &Entity<TerminalView>, cx: &mut VisualTestContext) {
        let prompt = |exit| SemanticMark::Prompt { exit };
        let screen =
            [("2", SemanticMark::Output), ("", SemanticMark::Output), ("$ ", prompt(Some(0)))];
        view.update_in(cx, |view, _window, cx| {
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
                    first_visible_line: LineIndex(6),
                    total_lines: 9,
                    input_ack: 0,
                    updates: screen
                        .iter()
                        .enumerate()
                        .map(|(row, (text, mark))| RowUpdate {
                            row: u16::try_from(row).unwrap(),
                            line: marked(text, *mark),
                        })
                        .collect(),
                }),
                cx,
            );
            view.apply(
                TermEvent::Lines {
                    start: LineIndex(0),
                    lines: vec![
                        marked("$ ls", prompt(None)),
                        marked("a", SemanticMark::Output),
                        marked("b", SemanticMark::Output),
                        marked("$ false", prompt(Some(0))),
                        marked("$ seq 2", prompt(Some(1))),
                        marked("1", SemanticMark::Output),
                    ],
                },
                cx,
            );
        });
        cx.run_until_parked();
    }

    fn top_line(view: &Entity<TerminalView>, cx: &VisualTestContext) -> LineIndex {
        view.read_with(cx, |view, _| view.state.index_at_row(0))
    }

    /// The view rows (0 = top) that carry a command-block separator, with its colour: the
    /// 1 px quads spanning the grid's width, read from the scene.
    fn separators(
        view: &Entity<TerminalView>,
        cx: &mut VisualTestContext,
    ) -> Vec<(u16, gpui::Hsla)> {
        let bounds = cx.debug_bounds("terminal").expect("the terminal is drawn");
        let metrics = view.read_with(cx, |view, _| view.metrics.expect("laid out"));
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        let near = |scaled: gpui::ScaledPixels, logical: Pixels| {
            f32::from(logical).mul_add(-scale, scaled.0).abs() < 0.5
        };
        let grid_width = metrics.cell_width * f32::from(metrics.cols);
        let mut out = Vec::new();
        for q in &quads {
            if !(near(q.bounds.size.height, px(1.0))
                && near(q.bounds.size.width, grid_width)
                && near(q.bounds.origin.x, metrics.origin.x))
            {
                continue;
            }
            assert!(q.bounds.origin.y.0 >= f32::from(bounds.origin.y) * scale, "inside the view");
            let row = (0..metrics.rows)
                .find(|&row| {
                    near(q.bounds.origin.y, metrics.origin.y + metrics.line_height * f32::from(row))
                })
                .expect("a separator sits on a row's top edge");
            out.push((row, q.background.as_solid().expect("a solid fill")));
        }
        out.sort_by_key(|(row, _)| *row);
        out
    }

    /// ⌘↑ / ⌘↓ put the previous / next prompt at the top of the viewport; the block separator
    /// is drawn on every prompt-start row but the first line, red after a failed command.
    #[gpui::test]
    fn cmd_up_and_down_walk_the_prompts_and_separators_follow(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        with_command_blocks(&view, cx);
        let theme = Theme::default();
        let ok = separator_color(&theme, Some(0));
        let failed = separator_color(&theme, Some(1));
        let none = separator_color(&theme, None);
        assert_ne!(ok, failed);
        assert_eq!(failed, hsla_alpha(theme.surfaces.error, alpha::SEPARATOR_ERROR));
        assert_eq!(ok, none, "no status and a zero status rule the same faint line");

        assert_eq!(top_line(&view, cx), LineIndex(6), "following output");
        assert_eq!(separators(&view, cx), vec![(2, ok)], "the newest prompt, after `seq 2`");

        cx.simulate_keystrokes("cmd-up");
        assert_eq!(top_line(&view, cx), LineIndex(4), "`$ seq 2` at the top");
        assert_eq!(separators(&view, cx), vec![(0, failed)], "`false` failed");

        cx.simulate_keystrokes("cmd-up");
        assert_eq!(top_line(&view, cx), LineIndex(3));
        assert_eq!(separators(&view, cx), vec![(0, ok), (1, failed)]);

        cx.simulate_keystrokes("cmd-up");
        assert_eq!(top_line(&view, cx), LineIndex(0));
        assert_eq!(separators(&view, cx), vec![], "never on the very first line");
        cx.simulate_keystrokes("cmd-up");
        assert_eq!(top_line(&view, cx), LineIndex(0), "nothing above: stays");

        cx.simulate_keystrokes("cmd-down");
        assert_eq!(top_line(&view, cx), LineIndex(3));
        cx.simulate_keystrokes("cmd-down");
        assert_eq!(top_line(&view, cx), LineIndex(4));
        cx.simulate_keystrokes("cmd-down");
        assert_eq!(top_line(&view, cx), LineIndex(6), "the newest prompt cannot go higher");
        assert!(view.read_with(cx, |view, _| view.state.view_offset() == 0), "following again");
    }

    /// The shaped-word cache: three rows made of two words shape two entries, another frame
    /// with the same words shapes nothing new, and a new word adds one.
    #[gpui::test]
    fn words_are_shaped_once_across_rows_and_frames(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let frame = |seq, rows: [&str; 3]| {
            TermEvent::Frame(Frame {
                seq,
                full: true,
                epoch: 0,
                cols: 10,
                rows: 3,
                cursor: Cursor::default(),
                modes: TermModes::empty(),
                oldest_line: LineIndex(0),
                first_visible_line: LineIndex(0),
                total_lines: 3,
                input_ack: 0,
                updates: rows
                    .iter()
                    .enumerate()
                    .map(|(row, text)| RowUpdate {
                        row: u16::try_from(row).unwrap(),
                        line: Line::from_text(text, 10, Style::DEFAULT),
                    })
                    .collect(),
            })
        };
        view.update_in(cx, |view, _window, cx| view.apply(frame(1, ["foo bar", "bar", "foo"]), cx));
        cx.run_until_parked();
        let words = |cx: &mut VisualTestContext| {
            cx.update(|_window, cx| crate::terminal::element::cached_words(cx))
        };
        assert_eq!(words(cx), 2, "foo and bar");
        view.update_in(cx, |view, _window, cx| view.apply(frame(2, ["bar foo", "foo", "bar"]), cx));
        cx.run_until_parked();
        assert_eq!(words(cx), 2, "the same words in other places shape nothing");
        view.update_in(cx, |view, _window, cx| view.apply(frame(3, ["bar foo", "baz", ""]), cx));
        cx.run_until_parked();
        assert_eq!(words(cx), 3, "baz is new; foo and bar are kept");
    }

    /// ⌘⇧C copies the output of the last finished command; with no marks it copies nothing.
    #[gpui::test]
    fn cmd_shift_c_copies_the_last_commands_output(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("before".into())));
        cx.simulate_keystrokes("cmd-shift-c");
        let text = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()))
        };
        assert_eq!(text(cx).as_deref(), Some("before"), "no prompts: the clipboard is untouched");

        with_command_blocks(&view, cx);
        cx.simulate_keystrokes("cmd-shift-c");
        assert_eq!(text(cx).as_deref(), Some("1\n2"), "blank tail trimmed, prompt rows excluded");
    }

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

    /// ⌘⇧L swaps the grid for the conversation and asks the host to follow the transcript;
    /// the host's slices fill it (a reset replaces, an append extends); ⌘⇧L again brings the
    /// grid back and stops the follow.
    #[gpui::test]
    fn the_conversation_replaces_the_grid_and_follows_the_transcript(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        while rx.try_recv().is_ok() {}
        assert!(cx.debug_bounds("conversation").is_none());

        cx.simulate_keystrokes("cmd-shift-l");
        let session = view.read_with(cx, |v, _| v.session);
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientMsg::Transcript(TranscriptFollow { session: s, follow: true })) if s == session
        ));
        assert!(cx.debug_bounds("conversation").is_some(), "the conversation is drawn");

        let snapshot = TranscriptUpdate {
            session,
            reset: true,
            entries: vec![user("fix it"), assistant("On it.")],
        };
        view.update(cx, |v, cx| v.transcript_update(snapshot, cx));
        let more = TranscriptUpdate { session, reset: false, entries: vec![tool("cargo test")] };
        view.update(cx, |v, cx| v.transcript_update(more, cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.conversation().map(|c| c.entries().len())), Some(3));
        let bounds = cx.debug_bounds("conversation").expect("drawn");
        assert!(bounds.size.width > px(0.0) && bounds.size.height > px(0.0));

        cx.simulate_keystrokes("cmd-shift-l");
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientMsg::Transcript(TranscriptFollow { follow: false, .. }))
        ));
        assert!(cx.debug_bounds("conversation").is_none(), "the grid is back");
        assert!(view.read_with(cx, |v, _| v.conversation().is_none()));
    }

    /// Whether the composer has the window's focus.
    fn composer_focused(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| view.read(cx).composer_focused(window, cx))
    }

    /// Every key the terminal received, oldest first, with the paste texts in between.
    fn drain_input(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ClientMsg::Term { req: TermRequest::Key(key), .. } => {
                    let mut name = format!("{:?}", key.code).to_lowercase();
                    if let Some(rest) = name.strip_prefix("key") {
                        name = rest.to_owned();
                    }
                    if key.mods.contains(slopty_proto::input::Mods::CTRL) {
                        name.insert_str(0, "ctrl-");
                    }
                    out.push(name);
                }
                ClientMsg::Term { req: TermRequest::Paste(text), .. } => {
                    out.push(format!("paste:{text}"));
                }
                _ => {}
            }
        }
        out
    }

    /// With the conversation on, typing lands in the composer and ↩ sends the text into the
    /// session as a paste followed by Enter, then clears it; ⇧↩ breaks a line instead; an
    /// empty ↩ is a bare Enter; Esc and ⌃C keep their terminal meaning; ⌘⇧L brings the grid
    /// back with the keyboard.
    #[gpui::test]
    fn the_composer_types_into_the_session(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        cx.simulate_keystrokes("cmd-shift-l");
        cx.run_until_parked();
        let _follow = drain_input(&mut rx);
        assert!(cx.debug_bounds("composer").is_some(), "the composer is drawn");
        assert!(composer_focused(&view, cx));

        cx.simulate_keystrokes("h i shift-enter y");
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some("hi\ny"));
        assert!(drain_input(&mut rx).is_empty(), "typing stays in the composer");

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(drain_input(&mut rx), ["paste:hi\ny", "enter"]);
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some(""), "the composer is empty again");

        cx.simulate_keystrokes("enter");
        assert_eq!(drain_input(&mut rx), ["enter"], "an empty submit is a bare Enter");

        cx.simulate_keystrokes("escape ctrl-c");
        assert_eq!(drain_input(&mut rx), ["escape", "ctrl-c"], "terminal keys pass through");
        assert!(composer_focused(&view, cx), "and keep the caret");

        cx.simulate_keystrokes("cmd-shift-l");
        cx.run_until_parked();
        assert!(cx.debug_bounds("composer").is_none());
        assert!(!composer_focused(&view, cx));
        cx.simulate_keystrokes("x");
        assert!(drain_input(&mut rx).contains(&"x".to_owned()), "the grid has the keys");
    }

    /// The border painted around the element with `selector`, from the scene.
    fn border_color_of(cx: &mut VisualTestContext, selector: &'static str) -> Option<gpui::Hsla> {
        let bounds = cx.debug_bounds(selector)?;
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        let near = |scaled: gpui::ScaledPixels, logical: Pixels| {
            f32::from(logical).mul_add(-scale, scaled.0).abs() < 1.0
        };
        quads
            .iter()
            .find(|q| {
                near(q.bounds.origin.x, bounds.origin.x)
                    && near(q.bounds.origin.y, bounds.origin.y)
                    && near(q.bounds.size.width, bounds.size.width)
                    && q.border_widths.top.0 > 0.0
            })
            .map(|q| q.border_color)
    }

    /// The composer's field wears the accent ring while it has the caret and a hairline
    /// once the grid takes the keyboard back; the row above it is the warn tint.
    #[gpui::test]
    fn the_composer_wears_a_focus_ring_only_while_focused(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let theme = Theme::default();
        cx.simulate_keystrokes("cmd-shift-l");
        cx.run_until_parked();
        assert!(composer_focused(&view, cx));
        assert_eq!(border_color_of(cx, "composer-field"), Some(hsla(theme.surfaces.accent)));

        cx.update(|window, cx| {
            let grid = view.read(cx).focus.clone();
            window.focus(&grid, cx);
        });
        cx.run_until_parked();
        assert!(!composer_focused(&view, cx));
        assert_eq!(border_color_of(cx, "composer-field"), Some(hsla(theme.surfaces.border)));

        let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
        view.update(cx, |v, cx| v.set_agent_status(Some(blocked), cx));
        cx.run_until_parked();
        let row = cx.debug_bounds("conversation-attention").expect("the attention row");
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        let fill = quads
            .iter()
            .find(|q| {
                f32::from(row.origin.y).mul_add(-scale, q.bounds.origin.y.0).abs() < 1.0
                    && f32::from(row.size.height).mul_add(-scale, q.bounds.size.height.0).abs()
                        < 1.0
            })
            .and_then(|q| q.background.as_solid());
        assert_eq!(fill, Some(hsla_alpha(theme.surfaces.warn, alpha::TINT)));
    }

    /// A permission puts an Allow / Deny row above the composer; pressing one raises
    /// `Answered` (the canvas types the key) and the row says so until the host reports the
    /// next state; a question puts the caret in the composer.
    #[gpui::test]
    fn the_attention_row_answers_a_permission(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let answers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&answers);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::Answered { allowed } = event {
                    seen.borrow_mut().push(*allowed);
                }
            })
            .detach();
        });
        cx.simulate_keystrokes("cmd-shift-l");
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation-attention").is_none());

        let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
        view.update(cx, |v, cx| v.set_agent_status(Some(blocked), cx));
        cx.run_until_parked();
        let allow = cx.debug_bounds("conversation-allow").expect("Allow is drawn");
        assert!(cx.debug_bounds("conversation-deny").is_some());
        cx.simulate_click(allow.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*answers.borrow(), [true]);
        assert!(cx.debug_bounds("conversation-allow").is_none(), "one tap, one answer");
        assert!(cx.debug_bounds("conversation-attention").is_some(), "the row says allowed");
        assert_eq!(
            view.read_with(cx, |v, _| v.attention()),
            Some(Attention::Permission { tool: "Bash".to_owned(), answered: Some(true) })
        );

        view.update(cx, |v, cx| v.set_agent_status(Some(AgentStatus::Working), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation-attention").is_none(), "the next state clears it");

        // The grid has the focus; a question moves the caret to the composer.
        cx.update(|window, cx| {
            let grid = view.read(cx).focus.clone();
            window.focus(&grid, cx);
        });
        cx.run_until_parked();
        assert!(!composer_focused(&view, cx));
        let question = AgentStatus::Blocked(BlockReason::Question);
        view.update(cx, |v, cx| v.set_agent_status(Some(question), cx));
        cx.run_until_parked();
        cx.run_until_parked();
        assert!(composer_focused(&view, cx));
        assert_eq!(view.read_with(cx, |v, _| v.attention()), Some(Attention::Prompt));
    }

    /// The list follows the tail until the reader scrolls up; then new entries leave the view
    /// where it is and a "↓ latest" pill appears, which pins it again; a reset pins too.
    #[gpui::test]
    fn the_list_stays_pinned_until_the_reader_scrolls_up(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        cx.simulate_keystrokes("cmd-shift-l");
        let session = view.read_with(cx, |v, _| v.session);
        let many: Vec<TranscriptEntry> = (0..60).map(|i| assistant(&format!("line {i}"))).collect();
        let update = |reset: bool, entries: Vec<TranscriptEntry>| TranscriptUpdate {
            session,
            reset,
            entries,
        };
        view.update(cx, |v, cx| v.transcript_update(update(true, many.clone()), cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(Conversation::pinned)));
        assert!(cx.debug_bounds("conversation-latest").is_none());
        let last = cx.debug_bounds("conversation-entry-59").expect("the last entry is in view");
        let list = cx.debug_bounds("conversation").expect("drawn");
        assert!(last.bottom() <= list.bottom());

        // Wheel up (positive y) over the list.
        let at = list.center();
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Lines(point(0.0, 6.0)),
            modifiers: gpui::Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| !c.pinned())));
        assert!(cx.debug_bounds("conversation-latest").is_some(), "the pill offers the way back");
        assert!(cx.debug_bounds("conversation-entry-59").is_none(), "the tail scrolled away");

        view.update(cx, |v, cx| v.transcript_update(update(false, vec![user("more")]), cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| !c.pinned())));
        assert!(cx.debug_bounds("conversation-entry-60").is_none(), "no yank");

        let pill = cx.debug_bounds("conversation-latest").expect("pill");
        cx.simulate_click(pill.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(Conversation::pinned)));
        assert!(cx.debug_bounds("conversation-latest").is_none());
        assert!(cx.debug_bounds("conversation-entry-60").is_some(), "back at the bottom");

        // Scroll up again, then a reset (new transcript) pins.
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Lines(point(0.0, 6.0)),
            modifiers: gpui::Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| !c.pinned())));
        view.update(cx, |v, cx| v.transcript_update(update(true, many), cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(Conversation::pinned)));
        assert!(cx.debug_bounds("conversation-entry-59").is_some());
    }

    /// Thinking and a tool's input start folded and open on a click; a result shows its
    /// first lines and the rest on a click; the fold state survives appends and not resets.
    #[gpui::test]
    fn folds_open_on_a_click(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        cx.simulate_keystrokes("cmd-shift-l");
        let session = view.read_with(cx, |v, _| v.session);
        let thinking = TranscriptEntry {
            at: None,
            body: TranscriptBody::Thinking { text: Clipped::whole("deep\nthought".to_owned()) },
        };
        let result = TranscriptEntry {
            at: Some(1_788_602_400_000),
            body: TranscriptBody::ToolResult {
                tool: Some("Bash".to_owned()),
                output: Clipped { text: "1\n2\n3\n4\n5\n6".to_owned(), more_lines: 7 },
                is_error: false,
            },
        };
        let update = TranscriptUpdate {
            session,
            reset: true,
            entries: vec![thinking, tool("cargo test"), result],
        };
        view.update(cx, |v, cx| v.transcript_update(update, cx));
        cx.run_until_parked();
        let folded = cx.debug_bounds("conversation-entry-0").expect("thinking");
        let tool_folded = cx.debug_bounds("conversation-entry-1").expect("tool");
        let result_folded = cx.debug_bounds("conversation-entry-2").expect("result");

        cx.simulate_click(folded.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| c.is_open(0))));
        let open = cx.debug_bounds("conversation-entry-0").expect("thinking");
        assert!(open.size.height > folded.size.height, "{open:?} vs {folded:?}");

        // The tool's header is its first line; the input opens under it.
        let header = point(tool_folded.center().x, tool_folded.top() + px(6.0));
        cx.simulate_click(header, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| c.is_open(1))));
        let open = cx.debug_bounds("conversation-entry-1").expect("tool");
        assert!(open.size.height > tool_folded.size.height);

        let header = point(result_folded.center().x, result_folded.top() + px(6.0));
        cx.simulate_click(header, gpui::Modifiers::default());
        cx.run_until_parked();
        let open = cx.debug_bounds("conversation-entry-2").expect("result");
        assert!(open.size.height > result_folded.size.height);

        // Clicking the header again folds; an append keeps the folds, a reset drops them.
        let header = point(open.center().x, open.top() + px(6.0));
        cx.simulate_click(header, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| !c.is_open(2))));
        let more = TranscriptUpdate { session, reset: false, entries: vec![user("ok")] };
        view.update(cx, |v, cx| v.transcript_update(more, cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| c.is_open(0))));
        let again = TranscriptUpdate { session, reset: true, entries: vec![user("ok")] };
        view.update(cx, |v, cx| v.transcript_update(again, cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| !c.is_open(0))));
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

    /// The conversation's attention row and composer are read with roles and labels, in
    /// reading order; Allow and Deny sit in the Tab ring and Enter answers.
    #[gpui::test]
    fn the_attention_row_and_the_composer_are_read_and_tabbed(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let answers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&answers);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::Answered { allowed } = event {
                    seen.borrow_mut().push(*allowed);
                }
            })
            .detach();
        });
        cx.simulate_keystrokes("cmd-shift-l");
        view.update(cx, |v, cx| {
            let session = v.session;
            let update = TranscriptUpdate {
                session,
                reset: true,
                entries: vec![user("hello"), assistant("hi")],
            };
            v.transcript_update(update, cx);
            let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
            v.set_agent_status(Some(blocked), cx);
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let at = |role: &str, label: &str| {
            tree.iter()
                .position(|n| n.is(role, Some(label)))
                .unwrap_or_else(|| panic!("{role} {label:?} in {tree:#?}"))
        };
        assert!(at("Group", "Conversation") < at("ListItem", "You: hello"));
        assert!(at("ListItem", "You: hello") < at("ListItem", "Claude: hi"));
        assert!(at("ListItem", "Claude: hi") < at("Status", "Claude wants to use Bash"));
        assert!(at("Status", "Claude wants to use Bash") < at("Button", "Allow"));
        assert!(at("Button", "Allow") < at("Button", "Deny"));
        assert!(at("Button", "Deny") < at("Group", "Composer"));
        assert!(at("Group", "Composer") < at("MultilineTextInput", "Message to Claude"));
        assert!(at("MultilineTextInput", "Message to Claude") < at("Button", "Send"));

        // From the grid, the first stop is Allow, then Deny; Enter on Deny answers.
        cx.update(|window, cx| {
            let grid = view.read(cx).focus.clone();
            window.focus(&grid, cx);
            window.focus_next(cx);
        });
        cx.run_until_parked();
        let focused_label = |cx: &mut VisualTestContext| {
            cx.update(|window, _| crate::a11y::tree(window))
                .into_iter()
                .find(|n| n.focused)
                .and_then(|n| n.label)
        };
        assert_eq!(focused_label(cx).as_deref(), Some("Allow"));
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        assert_eq!(focused_label(cx).as_deref(), Some("Deny"));
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent { keystroke: Keystroke::parse("enter").unwrap() });
        cx.run_until_parked();
        assert_eq!(*answers.borrow(), [false]);
    }

    /// The key bar's ⌘ arms exactly one tap: the next left press opens the link under it and
    /// disarms; a press with nothing under it disarms too and selects as usual.
    #[gpui::test]
    fn sticky_command_opens_the_link_under_the_next_tap(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
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
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    updates: vec![
                        RowUpdate {
                            row: 0,
                            line: Line::from_text("http://a.b", 10, Style::DEFAULT),
                        },
                        RowUpdate {
                            row: 1,
                            line: Line::from_text("http://c.d", 10, Style::DEFAULT),
                        },
                    ],
                }),
                cx,
            );
            view.set_sticky_command(true, cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.opened_url(), None);
        let metrics = view.read_with(cx, |v, _| v.metrics.expect("laid out"));
        let cell = |col: u16, row: u16| {
            metrics.origin
                + point(
                    metrics.cell_width * (f32::from(col) + 0.5),
                    metrics.line_height * (f32::from(row) + 0.5),
                )
        };
        cx.simulate_click(cell(2, 0), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(cx.opened_url().as_deref(), Some("http://a.b"));
        assert!(!view.read_with(cx, |v, _| v.sticky_command()), "one tap");
        // Disarmed, a press on the other link is a plain click: nothing opens.
        cx.simulate_click(cell(2, 1), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(cx.opened_url().as_deref(), Some("http://a.b"), "one tap, one link");
    }

    /// The grid is a terminal to a screen reader: its title as the label, the cursor row as
    /// the value, never the whole screen.
    #[gpui::test]
    fn the_grid_reads_its_cursor_row(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        with_command_blocks(&view, cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let grid =
            tree.iter().find(|n| n.role == "Terminal").unwrap_or_else(|| panic!("{tree:#?}"));
        assert_eq!(grid.label.as_deref(), Some("shell"));
        assert_eq!(grid.value.as_deref(), Some("2"), "row 0 holds the cursor: {grid:?}");
        assert_eq!(view.read_with(cx, |v, _| v.cursor_row_text()), "2");
    }

    /// Underline and strikethrough are drawn from the font's own metrics — whole device
    /// pixels, measured down from the top of the row — instead of GPUI's fixed offsets: the
    /// strikethrough crosses the text, the underline sits below it, both inside the row.
    #[gpui::test]
    fn the_decorations_sit_where_the_font_puts_them(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let style = Style {
            underline: slopty_grid::Underline::Single,
            flags: slopty_grid::StyleFlags::STRIKETHROUGH,
            ..Style::DEFAULT
        };
        view.update_in(cx, |view, _window, cx| {
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
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    updates: vec![RowUpdate { row: 0, line: Line::from_text("abc", 10, style) }],
                }),
                cx,
            );
        });
        cx.run_until_parked();

        let metrics = view.read_with(cx, |view, _| view.metrics.expect("laid out"));
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        let width = f32::from(metrics.cell_width * 3.0) * scale;
        let top = f32::from(metrics.origin.y) * scale;
        // The two strokes: the only quads three cells wide at the left of the first row.
        let mut strokes: Vec<(f32, f32)> = quads
            .iter()
            .filter(|q| {
                (q.bounds.size.width.0 - width).abs() < 0.5
                    && f32::from(metrics.origin.x).mul_add(-scale, q.bounds.origin.x.0).abs() < 0.5
            })
            .map(|q| (q.bounds.origin.y.0 - top, q.bounds.size.height.0))
            .collect();
        strokes.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(strokes.len(), 2, "a strikethrough and an underline: {strokes:?}");

        let row = f32::from(metrics.line_height) * scale;
        for (y, thickness) in &strokes {
            assert!(thickness.fract() == 0.0 && *thickness >= 1.0, "whole pixels: {thickness}");
            assert!(y.fract() == 0.0, "on a device pixel: {y}");
            assert!(*y > 0.0 && y + thickness <= row, "inside the row: {y} + {thickness} > {row}");
        }
        let (strikethrough, underline) = (strokes[0].0, strokes[1].0);
        assert!(
            strikethrough > row * 0.25 && strikethrough < row * 0.75,
            "the strikethrough crosses the lowercase letters: {strokes:?}"
        );
        assert!(underline > row * 0.75, "the underline is below the baseline: {strokes:?}");
    }

    /// However far the canvas has zoomed, the painted grid stays inside the item: the columns
    /// were counted with the unzoomed cell, so the zoomed cell is that one scaled, never one
    /// derived again and rounded up (which clipped the last column at small zooms).
    #[gpui::test]
    fn a_zoomed_grid_still_fits_the_item(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let pad = px(Theme::default().spacing.sm);
        for zoom in [0.3_f32, 0.5, 1.0, 2.0] {
            view.update_in(cx, |view, _window, cx| {
                view.set_zoom(zoom);
                cx.notify();
            });
            cx.run_until_parked();
            let item = cx.debug_bounds("terminal").expect("the terminal is drawn");
            let m = view.read_with(cx, |view, _| view.metrics.expect("laid out"));
            let content = item.size.width - pad * 2.0 * zoom;
            let painted = m.cell_width * f32::from(m.cols);
            assert!(painted <= content, "at zoom {zoom}: {painted:?} > {content:?}");
            let rows = m.line_height * f32::from(m.rows);
            let tall = item.size.height - pad * 2.0 * zoom;
            assert!(rows <= tall, "at zoom {zoom}: {rows:?} > {tall:?}");
        }
    }
}
