//! `TerminalView`: one attached session on screen.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Autocapitalize, Bounds, Context, CursorStyle, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding,
    KeyDownEvent, Keystroke, LongPressEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, TextInputAction,
    TextInputConfiguration, TouchPhase, UTF16Selection, Window, anchored, deferred, div, point, px,
    size,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::term::{BlockHead, CommandBlock, TermImage};
use slopty_client::{Effect, TermState};
use slopty_core::SessionId;
use slopty_grid::{Cursor, LineIndex, TermModes};
use slopty_predict::{Policy, Prediction, Predictor};
use slopty_proto::ClientMsg;
use slopty_proto::agent::AgentStatus;
use slopty_proto::input::{MouseAction, MouseButton as ProtoButton, MouseEvent};
use slopty_proto::terminal::{Placement, SearchMatch, TermEvent, TermRequest, TermSize};
use slopty_theme::{Theme, alpha};
use tokio::sync::mpsc;

use crate::colors::{hsla, hsla_alpha};
use crate::keys;
use crate::kit::FIND_PLACEHOLDER;
use crate::terminal::element::{CellMetrics, TerminalElement, separator_color};
use crate::terminal::{latency, url};

/// Hits asked for per search; the host counts every hit regardless.
const SEARCH_MAX: u32 = 5_000;
/// While the search bar is open, output refreshes the hits at most this often.
const SEARCH_REFRESH: Duration = Duration::from_millis(300);
/// How often a selection dragged past the grid's edge scrolls, and the most lines one tick
/// moves (the pointer's distance past the edge picks the pace, one line per row of distance).
const AUTOSCROLL_TICK: Duration = Duration::from_millis(50);
/// Whether a paste can go straight to the program (ghostty's `clipboard-paste-protection`):
/// outside bracketed paste a newline runs whatever precedes it, inside it the end sequence
/// closes the bracket and the rest is typed. Either waits for a confirmation.
#[must_use]
pub fn paste_is_safe(text: &str, bracketed: bool) -> bool {
    if bracketed { !text.contains("\x1b[201~") } else { !text.contains(['\n', '\r']) }
}

/// Half a blink: the cursor (and SGR 5 text) shows for this long, then hides for as long.
/// Ghostty's cadence.
const BLINK_HALF: Duration = Duration::from_millis(600);
/// How long the view flashes for a bell (BEL): a tint over the grid, gone before it annoys.
const BELL_FLASH: Duration = Duration::from_millis(150);
/// Half of it, for the tests.
#[cfg(test)]
const BELL_HALF: Duration = Duration::from_millis(75);
const AUTOSCROLL_MAX: i64 = 8;
/// How far a ⌘-press on a path moves, in points, before it is a drag of the file rather than
/// a click that opens it.
const DRAG_OUT_SLOP: f32 = 4.0;

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
            /// Scroll a page up into history.
            ScrollPageUp,
            /// Scroll a page down towards the output.
            ScrollPageDown,
            /// Scroll to the oldest line the host keeps.
            ScrollToTop,
            /// Back to following the output.
            ScrollToBottom,
            /// Select every line, history included.
            SelectAll,
            /// Copy the output of the last command.
            CopyLastOutput,
            /// Run the last command again: a paste of what was typed, then ↩.
            RerunLast,
            /// Save the last command and its output as a note card beside the shell.
            NoteLastBlock,
            /// Clear the screen and the history (⌘K, as in every Mac terminal).
            ClearScreen,
        ]
    );
}
pub use actions::{
    ClearScreen, CloseFind, Copy, CopyLastOutput, Find, FindNext, FindPrev, NextPrompt,
    NoteLastBlock, Paste, PrevPrompt, RerunLast, ScrollPageDown, ScrollPageUp, ScrollToBottom,
    ScrollToTop, SelectAll,
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
        // ghostty's scroll keys; the ⌘ pair is what Terminal.app taught the Mac.
        KeyBinding::new("shift-pageup", ScrollPageUp, CTX),
        KeyBinding::new("shift-pagedown", ScrollPageDown, CTX),
        KeyBinding::new("shift-home", ScrollToTop, CTX),
        KeyBinding::new("shift-end", ScrollToBottom, CTX),
        KeyBinding::new("cmd-home", ScrollToTop, CTX),
        KeyBinding::new("cmd-end", ScrollToBottom, CTX),
        KeyBinding::new("cmd-a", SelectAll, CTX),
        KeyBinding::new("cmd-shift-c", CopyLastOutput, CTX),
        // ⌘⇧↩ is the workspace's maximize-column; "Rerun last command" is in the palette.
        KeyBinding::new("cmd-k", ClearScreen, CTX),
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
    /// A rectangle between the two corners (⌥-drag) rather than a run in reading order.
    pub block: bool,
}

impl Selection {
    /// `(start, end)` in reading order.
    #[must_use]
    pub fn ordered(self) -> ((LineIndex, u16), (LineIndex, u16)) {
        if self.head < self.anchor { (self.head, self.anchor) } else { (self.anchor, self.head) }
    }

    /// A run between two cells, in reading order.
    #[must_use]
    pub const fn run(anchor: (LineIndex, u16), head: (LineIndex, u16)) -> Self {
        Self { anchor, head, block: false }
    }

    /// The selected columns on line `index` as `start..end`, if the line is inside the
    /// selection; a line in the middle is selected edge to edge, or between the rectangle's
    /// sides when the selection is a block.
    #[must_use]
    pub fn columns(self, index: LineIndex, cols: u16) -> Option<std::ops::Range<u16>> {
        let (start, end) = self.ordered();
        if index < start.0 || index > end.0 {
            return None;
        }
        let (from, to) = if self.block {
            let (a, b) = (self.anchor.1.min(self.head.1), self.anchor.1.max(self.head.1));
            (a, b.saturating_add(1).min(cols))
        } else {
            (
                if index == start.0 { start.1 } else { 0 },
                if index == end.0 { end.1.saturating_add(1).min(cols) } else { cols },
            )
        };
        (from < to).then_some(from..to)
    }
}

/// Things the surrounding UI may want to react to.
#[derive(Clone, Debug)]
pub enum TerminalViewEvent {
    /// Title changed.
    Title(String),
    /// The program asked for a desktop notification (OSC 9 / 777 / 99).
    Notification {
        /// Its title, possibly empty.
        title: String,
        /// Its body.
        body: String,
    },
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
    /// Something the human should read in the top bar for a moment (a picture refused).
    Notice(String),
    /// The human confirmed closing this shell while its command runs: the canvas sends
    /// the host `Close`.
    CloseConfirmed,
    /// A shell command finished (shell integration marks): what was typed, its status, and how
    /// long it ran from the frame the cursor left its prompt to the frame the next prompt
    /// arrived. The canvas badges the item when that was long and nobody was watching.
    CommandFinished {
        /// What was typed.
        command: String,
        /// Its exit status, when the shell said.
        exit: Option<u8>,
        /// How long it ran.
        elapsed: Duration,
    },
    /// "Save as note" on a block's menu: the block as a note's Markdown (`block_note`); the
    /// canvas puts a note card with it beside the shell.
    NoteBlock(String),
    /// "View" on a tool call that named a file, or ⌘-click on a path while a command runs:
    /// the canvas opens (or reveals) a file card for it, a relative path made absolute
    /// against the session's directory, landing on `line` (1-based) when one is known.
    ViewFile {
        /// The path as the tool or the text gave it.
        path: String,
        /// The line to land on.
        line: Option<u32>,
    },
    /// ⌘-drag on a path: the canvas drags that file out of the app (a file promise the
    /// worker keeps), a relative path made absolute against the session's directory.
    DragOut {
        /// The path as the text gave it.
        path: String,
    },
}

/// A placed image with the texture the element paints it from.
#[derive(Clone, Debug)]
pub struct PlacedImage {
    /// Where the host laid it out.
    pub placement: Placement,
    /// The texture, BGRA premultiplied as GPUI wants it.
    pub image: Arc<gpui::RenderImage>,
    /// The texture's size in pixels.
    pub width: u32,
    /// The texture's size in pixels.
    pub height: u32,
}

/// The texture made of one image id, at the generation it was made from.
struct Texture {
    generation: u64,
    image: Arc<gpui::RenderImage>,
}

/// One session's view.
pub struct TerminalView {
    session: SessionId,
    state: TermState,
    /// One texture per placed image, made when its pixels arrive, dropped with them.
    textures: HashMap<u32, Texture>,
    out: mpsc::Sender<ClientMsg>,
    focus: FocusHandle,
    theme: Theme,
    key_seq: u64,
    metrics: Option<CellMetrics>,
    pending_size: Option<TermSize>,
    font_family: Option<SharedString>,
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
    /// This press made or moved the selection: when the theme says copy on select, the
    /// release copies it.
    selected_by_press: bool,
    /// A plain left press and its cell: released without a drag, it moves the shell's cursor
    /// there.
    click_at: Option<(LineIndex, u16)>,
    /// A ⌘-press on a path, acted on when the button comes up (open it) or when the pointer
    /// moves off far enough first (drag the file out).
    path_press: Option<(url::PathSpan, bool, gpui::Point<Pixels>)>,
    /// The selection is being dragged past the grid's top or bottom: lines to scroll each
    /// tick (positive = up into history) and the column the pointer holds.
    autoscroll: Option<(i64, u16)>,
    /// The ticking loop behind `autoscroll`; dropped (cancelled) when a new drag starts one.
    autoscroll_task: Option<gpui::Task<()>>,
    /// The blink clock: the phase (true = shown), whether the last painted frame had anything
    /// to blink, when a keystroke last pinned the phase on, and the ticking loop while wanted.
    blink_on: bool,
    blink_wanted: bool,
    blink_pinned: Option<Instant>,
    blink_task: Option<gpui::Task<()>>,
    /// The visual bell: the grid is tinted while the task waits out [`BELL_FLASH`]; a new
    /// bell restarts it (dropping the old task cancels it).
    bell_flash: bool,
    bell_task: Option<gpui::Task<()>>,
    /// The scrollbar's thumb is held: the pointer's offset from the thumb's top.
    thumb_drag: Option<Pixels>,
    /// The fraction of a line the wheel has moved short of a whole one (a trackpad scrolls
    /// in fractions; they add up).
    wheel_remainder: f32,
    /// Whether the gesture in flight belongs to the grid, decided by its first movement and
    /// held through its momentum until the next one starts. `None` while a gesture has yet to
    /// move; a mouse wheel never reads or writes it.
    wheel_gesture: Option<bool>,
    /// The command-block menu a right click opened, and where.
    block_menu: Option<BlockMenu>,
    /// When the running shell command left its prompt.
    command_started: Option<Instant>,
    /// How long each finished command took, by the row it was typed at, for the caption at
    /// the right end of that row; only those at or over [`TOOK_MIN`]. Rows are numbered per
    /// epoch, so a new epoch empties it (`took_epoch` remembers which one filled it).
    took: HashMap<LineIndex, Duration>,
    took_epoch: Option<u32>,
    /// The cell under the pointer, for the ⌘-hover link underline.
    hover: Option<(u16, u16)>,
    /// ⌘ is down: links under the pointer show as links.
    cmd_held: bool,
    /// ⇧ is held: the pointer is the human's even while a program reports the mouse.
    shift_held: bool,
    /// A long press claimed the touch; moving the finger extends the selection.
    touch_selecting: bool,
    /// The search bar, while open.
    search: Option<Search>,
    /// The agent's state in this session, as the host last reported it.
    agent: Option<AgentStatus>,
    /// The search mode the next bar opens with (regex or plain).
    search_regex: bool,
    /// What waits on a confirmation at the card's foot: a paste held back by paste
    /// protection, or the close of a shell whose command is still running.
    pending: Option<Pending>,
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
            textures: HashMap::new(),
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
            block_menu: None,
            command_started: None,
            took: HashMap::new(),
            took_epoch: None,
            selection: None,
            selecting: false,
            selected_by_press: false,
            click_at: None,
            path_press: None,
            hover: None,
            cmd_held: false,
            shift_held: false,
            touch_selecting: false,
            autoscroll: None,
            autoscroll_task: None,
            blink_on: true,
            blink_wanted: false,
            blink_pinned: None,
            blink_task: None,
            bell_flash: false,
            bell_task: None,
            thumb_drag: None,
            wheel_remainder: 0.0,
            wheel_gesture: None,
            search: None,
            agent: None,
            search_regex: false,
            pending: None,
        }
    }

    /// Type `text` into this session and run it: a paste (bracketed when the shell asks, as
    /// any paste, so a multi-line block arrives whole), then ↩ as a key, the way the human
    /// would have. Shared by the block menu's "rerun" and the canvas's "run in shell".
    pub fn run_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.send(TermRequest::Paste(text));
        if let Ok(enter) = Keystroke::parse("enter") {
            self.press(enter, cx);
        }
    }

    /// The host's word on the agent in this session (`None`: no agent).
    pub fn set_agent_status(&mut self, status: Option<AgentStatus>, cx: &mut Context<Self>) {
        self.agent = status;
        cx.notify();
    }

    /// The agent's state as last reported.
    #[must_use]
    pub const fn agent_status(&self) -> Option<&AgentStatus> {
        self.agent.as_ref()
    }

    /// ⌘F: open the search bar, or put the caret back in it with the text selected.
    pub fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(FIND_PLACEHOLDER));
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

    /// Open the find bar on `needle` (a find in every card chose this one): the field holds
    /// it and the newest hit is revealed when the host answers.
    pub fn find_with(&mut self, needle: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.find(&Find, window, cx);
        let Some(search) = &mut self.search else { return };
        search.input.update(cx, |input, cx| input.set_value(needle.to_owned(), window, cx));
        needle.clone_into(&mut search.needle);
        self.restart_search(cx);
    }

    /// The find bar's needle, when the bar is open.
    #[must_use]
    pub fn search_needle(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.needle.as_str())
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
        let n = search.matches.len();
        if n == 0 {
            return;
        }
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
        url::link_at_col(line, col)
            .map(|span| (index, span.start, span.end))
            .or_else(|| url::path_at_col(line, col).map(|span| (index, span.start, span.end)))
    }

    /// ⌘-click on a file path: open it in the shell's editor (`$EDITOR`, else `vi`, at the
    /// line the text named) by typing the command at the prompt. While a command runs the
    /// prompt is not there to type at, so the path opens as a file card on the canvas
    /// instead (the canvas makes it absolute against this shell's directory); so does a tap
    /// the key bar's ⌘ armed (`sticky`), since a phone has no comfortable editor to type into.
    fn open_path(&mut self, span: &url::PathSpan, sticky: bool, cx: &mut Context<Self>) {
        if sticky || self.state.command_running() {
            tracing::info!(path = %span.path, sticky, "path viewed");
            cx.emit(TerminalViewEvent::ViewFile { path: span.path.clone(), line: span.line });
            return;
        }
        let command = url::editor_command(&span.path, span.line);
        tracing::info!(path = %span.path, line = ?span.line, "open path");
        self.run_text(command, cx);
    }

    /// Where the link under a ⌘-hover goes: the OSC 8 target or the URL as printed, or the
    /// path (with its `:line`) — the text a click would act on, shown so an OSC 8 label
    /// cannot hide its destination.
    #[must_use]
    pub fn link_target(&self) -> Option<String> {
        if !self.cmd_held {
            return None;
        }
        let (col, row) = self.hover?;
        let line = self.state.line(self.state.index_at_row(row))?;
        if let Some(link) = url::link_at_col(line, col) {
            return Some(link.url);
        }
        let path = url::path_at_col(line, col)?;
        Some(path.line.map_or_else(|| path.path.clone(), |n| format!("{}:{n}", path.path)))
    }

    /// The chip at the card's bottom-left naming the ⌘-hovered link's target.
    /// The strip that asks before a paste that could run: what it holds, Paste and Cancel.
    fn render_confirm(&self, cx: &Context<Self>) -> Option<gpui::Div> {
        let pending = self.pending.as_ref()?;
        let theme = &self.theme;
        let (s, spacing, radii) = (&theme.surfaces, theme.spacing, theme.radii);
        let (what, selector, accept, accept_id, cancel_id) = match pending {
            Pending::Paste(text) => {
                let lines = text.lines().count().max(1);
                let what = if lines == 1 {
                    "Paste a line that would run?".to_owned()
                } else {
                    format!("Paste {lines} lines that would run?")
                };
                (what, "paste-confirm", "Paste", "terminal-paste-confirm", "terminal-paste-cancel")
            }
            Pending::Close(command) => (
                format!("Close while `{command}` runs?"),
                "close-confirm",
                "Close",
                "terminal-close-confirm",
                "terminal-close-cancel",
            ),
        };
        let button = move |id: &'static str, label: &'static str, accent: bool| {
            let row = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(gpui::accesskit::Role::Button)
                .aria_label(label)
                .px(px(spacing.sm))
                .py(px(spacing.xxs))
                .rounded(px(radii.xs))
                .cursor_pointer()
                .when(accent, |el| el.bg(hsla(s.accent)).text_color(hsla(s.accent_fg)))
                .when(!accent, |el| {
                    el.text_color(hsla(s.text_secondary))
                        .hover(move |el| el.bg(hsla_alpha(s.text, alpha::FAINT)))
                })
                .child(label);
            crate::a11y::tab_stop(row, s.accent)
        };
        Some(
            div()
                .debug_selector(move || selector.to_owned())
                .absolute()
                .bottom(px(spacing.xs))
                .left(px(spacing.xs))
                .occlude()
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .px(px(spacing.sm))
                .py(px(spacing.xs))
                .rounded(px(radii.sm))
                .bg(hsla(s.panel))
                .border_1()
                .border_color(hsla(s.border))
                .shadow_sm()
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text))
                .font_family(theme.typography.ui_family.clone())
                .child(SharedString::from(what))
                .child(
                    button(accept_id, accept, true)
                        .on_click(cx.listener(|this, _ev, _window, cx| this.confirm_pending(cx))),
                )
                .child(
                    button(cancel_id, "Cancel", false)
                        .on_click(cx.listener(|this, _ev, _window, cx| this.cancel_pending(cx))),
                ),
        )
    }

    fn render_link_preview(&self) -> Option<gpui::Div> {
        let target = self.link_target()?;
        let theme = &self.theme;
        let (s, spacing, radii) = (&theme.surfaces, theme.spacing, theme.radii);
        Some(
            div()
                .debug_selector(|| "link-preview".to_owned())
                .absolute()
                .bottom(px(spacing.xs))
                .left(px(spacing.xs))
                .max_w_full()
                .px(px(spacing.sm))
                .py(px(spacing.xs))
                .rounded(px(radii.xs))
                .bg(hsla(s.panel))
                .border_1()
                .border_color(hsla(s.border))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .font_family(theme.typography.ui_family.clone())
                .overflow_hidden()
                .whitespace_nowrap()
                .child(SharedString::from(target)),
        )
    }

    /// The pointer's shape over the grid: an I-beam over text, a hand over the link ⌘ would
    /// open, and an arrow while a program has the mouse (⇧ takes it back, as a click does).
    #[must_use]
    pub fn pointer(&self) -> CursorStyle {
        if self.link_highlight().is_some() {
            return CursorStyle::PointingHand;
        }
        if self.state.modes().contains(TermModes::MOUSE_TRACKING) && !self.shift_held {
            return CursorStyle::Arrow;
        }
        CursorStyle::IBeam
    }

    /// Pointer position and modifier state changed; repaint only when the underline or the
    /// pointer's shape moves.
    fn set_pointer(
        &mut self,
        hover: Option<(u16, u16)>,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let before = (self.link_highlight(), self.pointer());
        self.hover = hover;
        self.cmd_held = modifiers.platform;
        self.shift_held = modifiers.shift;
        if (self.link_highlight(), self.pointer()) != before {
            cx.notify();
        }
    }

    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_pointer(self.hover, event.modifiers, cx);
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
        self.selection = Some(Selection::run((index, start), (index, end)));
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
            TouchPhase::Ended => {
                let selecting = std::mem::take(&mut self.touch_selecting);
                if selecting {
                    self.copy_on_select(cx);
                }
                selecting
            }
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

    /// The menu's items, in order: what applies to the block under the click, then the
    /// terminal's own (Copy only with something selected).
    fn block_menu_items(block: Option<&CommandBlock>, has_selection: bool) -> Vec<BlockMenuItem> {
        let mut items = Vec::new();
        if let Some(block) = block {
            if block.command.is_some() {
                items.push(BlockMenuItem::CopyCommand);
            }
            if !block.output.is_empty() {
                items.push(BlockMenuItem::CopyOutput);
            }
            if block.command.is_some() {
                items.push(BlockMenuItem::Rerun);
            }
            items.push(BlockMenuItem::Note);
            items.push(BlockMenuItem::SelectBlock);
        }
        if has_selection {
            items.push(BlockMenuItem::Copy);
            if block.is_none() {
                items.push(BlockMenuItem::Note);
            }
        }
        items.push(BlockMenuItem::Paste);
        items.push(BlockMenuItem::Find);
        items.push(BlockMenuItem::ClearScreen);
        items
    }

    /// A menu item was chosen: do it and close the menu.
    fn block_menu_pick(
        &mut self,
        item: BlockMenuItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.block_menu.take() else { return };
        match item {
            BlockMenuItem::Copy => self.copy(&Copy, window, cx),
            BlockMenuItem::Paste => self.paste_clipboard(&Paste, window, cx),
            BlockMenuItem::Find => self.find(&Find, window, cx),
            BlockMenuItem::ClearScreen => self.clear_screen(&ClearScreen, window, cx),
            BlockMenuItem::Note => {
                let text = match menu.block {
                    Some(block) => block_note(&block),
                    None => match self.selected_text().filter(|t| !t.trim().is_empty()) {
                        Some(t) => format!("```\n{t}\n```\n"),
                        None => return,
                    },
                };
                cx.emit(TerminalViewEvent::NoteBlock(text));
            }
            BlockMenuItem::CopyCommand
            | BlockMenuItem::CopyOutput
            | BlockMenuItem::Rerun
            | BlockMenuItem::SelectBlock => {
                if let Some(block) = menu.block {
                    self.block_item_pick(item, block, cx);
                }
            }
        }
        cx.notify();
    }

    /// One of the block's own items.
    fn block_item_pick(
        &mut self,
        item: BlockMenuItem,
        block: CommandBlock,
        cx: &mut Context<Self>,
    ) {
        match item {
            BlockMenuItem::CopyCommand => {
                if let Some(command) = block.command {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(command));
                }
            }
            BlockMenuItem::CopyOutput => {
                if !block.output.is_empty() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(block.output));
                }
            }
            BlockMenuItem::Rerun => {
                if let Some(command) = block.command {
                    self.run_text(command, cx);
                }
            }
            BlockMenuItem::SelectBlock => {
                let last = LineIndex(block.end.0.saturating_sub(1).max(block.prompt.0));
                let cols = self.state.size().cols;
                self.selection =
                    Some(Selection::run((block.prompt, 0), (last, cols.saturating_sub(1))));
                self.selecting = false;
            }
            BlockMenuItem::Copy
            | BlockMenuItem::Paste
            | BlockMenuItem::Find
            | BlockMenuItem::ClearScreen
            | BlockMenuItem::Note => {}
        }
    }

    /// ⇧-arrow with a selection: where its head moves (a cell sideways, wrapping at the row's
    /// ends; a row up or down, within the lines the host keeps). `None` when the keystroke is
    /// not that.
    fn adjusted_head(
        &self,
        selection: Selection,
        keystroke: &Keystroke,
    ) -> Option<(LineIndex, u16)> {
        let m = keystroke.modifiers;
        if !m.shift || m.control || m.alt || m.platform {
            return None;
        }
        let last_col = self.state.size().cols.saturating_sub(1);
        let oldest = self.state.scrollback().oldest();
        let newest = LineIndex(self.state.scrollback().total().saturating_sub(1).max(oldest.0));
        let (line, col) = selection.head;
        Some(match keystroke.key.as_str() {
            "left" if col > 0 => (line, col.saturating_sub(1)),
            "left" if line > oldest => (LineIndex(line.0.saturating_sub(1)), last_col),
            "right" if col < last_col => (line, col.saturating_add(1)),
            "right" if line < newest => (LineIndex(line.0.saturating_add(1)), 0),
            "left" | "right" => (line, col),
            "up" => (LineIndex(line.0.saturating_sub(1).max(oldest.0)), col),
            "down" => (LineIndex(line.0.saturating_add(1).min(newest.0)), col),
            _ => return None,
        })
    }

    /// The block menu, drawn late and anchored where the right click landed.
    /// The command whose block the viewport's top row is inside while every row of its
    /// prompt has scrolled above: what the sticky header shows. `None` on a prompt row, off
    /// a block, or for a block without a typed command. Read every frame, so only the block's
    /// head (its prompt rows), never its output.
    #[must_use]
    pub fn block_header(&self) -> Option<BlockHead> {
        let top = self.state.index_at_row(0);
        if self.state.line(top).is_none_or(|line| line.mark.is_prompt()) {
            return None;
        }
        let head = self.state.block_head(top)?;
        (head.prompt < top && head.command.as_deref().is_some_and(|c| !c.is_empty()))
            .then_some(head)
    }

    /// One row over the grid's top naming the command whose output the viewport is inside,
    /// so a long output is never anonymous; a click scrolls its prompt back to the top. The
    /// hairline under it is the block's separator colour, red after a failure.
    fn render_block_header(
        &self,
        block: &BlockHead,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let metrics = self.metrics?;
        let command = block.command.clone()?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let prompt = block.prompt;
        // The family the grid resolved, so the header's text is the grid's.
        let family = self.font_family.clone().unwrap_or_else(|| {
            theme.typography.mono_families.first().cloned().unwrap_or_default().into()
        });
        // The block's duration at the right end, as its prompt row would show it.
        let took = self.took(prompt).map(|elapsed| {
            div()
                .debug_selector(|| "block-header-took".to_owned())
                .flex_none()
                .pl(px(theme.spacing.sm))
                .child(SharedString::from(took_label(elapsed)))
        });
        Some(
            div()
                .id("block-header")
                .debug_selector(|| "block-header".to_owned())
                .role(gpui::accesskit::Role::Button)
                .aria_label(command.clone())
                .absolute()
                .top_0()
                .left_0()
                .w_full()
                .h(metrics.line_height)
                .flex()
                .items_center()
                .justify_between()
                .px(px(theme.spacing.sm))
                .bg(hsla(s.panel))
                .border_b_1()
                .border_color(separator_color(theme, block.exit))
                .font_family(family)
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .cursor_pointer()
                .overflow_hidden()
                .whitespace_nowrap()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.jump_to(prompt, cx);
                    }),
                )
                .child(div().overflow_hidden().child(command))
                .children(took)
                .into_any_element(),
        )
    }

    fn render_block_menu(&self, menu: &BlockMenu, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = &theme.spacing;
        let has_selection = self.selected_text().is_some_and(|t| !t.is_empty());
        let items = Self::block_menu_items(menu.block.as_ref(), has_selection);
        let list = div()
            .id("block-menu")
            .debug_selector(|| "block-menu".to_owned())
            .role(gpui::accesskit::Role::Menu)
            .aria_label(if menu.block.is_some() { "Command block" } else { "Terminal" })
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(160.0))
            .p(px(spacing.xs))
            .rounded(px(theme.radii.sm))
            .border_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .shadow_sm()
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
                this.block_menu = None;
                cx.notify();
            }))
            .children(items.into_iter().map(|item| {
                let key = item.key();
                let row = div()
                    .id(gpui::ElementId::Name(format!("block-menu-{key}").into()))
                    .debug_selector(move || format!("block-menu-{key}"))
                    .role(gpui::accesskit::Role::MenuItem)
                    .aria_label(item.label())
                    .px(px(spacing.sm))
                    .py(px(spacing.xxs))
                    .rounded(px(theme.radii.xs))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla_alpha(s.accent, alpha::PRESSED)))
                    .child(SharedString::from(item.label()));
                crate::a11y::tab_stop(row, s.accent).on_click(cx.listener(
                    move |this, _ev, window, cx| {
                        this.block_menu_pick(item, window, cx);
                    },
                ))
            }));
        deferred(anchored().position(menu.at).snap_to_window_with_margin(px(8.0)).child(list))
            .with_priority(1)
            .into_any_element()
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

    /// ⌘⇧↩: the last finished command again, typed as the block menu's rerun types it.
    pub fn rerun_last(&mut self, _: &RerunLast, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.state.last_command() {
            self.run_text(command, cx);
        }
    }

    /// The palette's "Keep last block as a card": the last finished command's block (the one
    /// before the newest prompt) as a note card beside the shell; nothing without one.
    pub fn note_last_block(
        &mut self,
        _: &NoteLastBlock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(block) = self.state.last_block() {
            cx.emit(TerminalViewEvent::NoteBlock(block_note(&block)));
        }
    }

    /// ⌘K: the host drops the history and the shell repaints its prompt at the top.
    pub fn clear_screen(&mut self, _: &ClearScreen, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.scroll_to_bottom();
        self.send(TermRequest::Clear);
        cx.notify();
    }

    /// ⌘↑: the prompt above the viewport's top row, scrolled to the top.
    pub fn prev_prompt(&mut self, _: &PrevPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_prompt(-1, cx);
    }

    /// ⌘↓: the prompt below the viewport's top row, scrolled to the top; none left means back
    /// to following output.
    pub fn next_prompt(&mut self, _: &NextPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_prompt(1, cx);
    }

    /// ⇧⇞: a page up into history.
    pub fn scroll_page_up(
        &mut self,
        _: &ScrollPageUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_lines(i64::from(self.state.size().rows).max(1), cx);
    }

    /// ⇧⇟: a page down towards the output.
    pub fn scroll_page_down(
        &mut self,
        _: &ScrollPageDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_lines(i64::from(self.state.size().rows).max(1).saturating_neg(), cx);
    }

    /// ⇧⇱ / ⌘⇱: the oldest line the host keeps at the top.
    pub fn scroll_to_top(&mut self, _: &ScrollToTop, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_lines(i64::MAX, cx);
    }

    /// ⇧⇲ / ⌘⇲: back to following the output.
    pub fn scroll_to_bottom(
        &mut self,
        _: &ScrollToBottom,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_lines(i64::MIN, cx);
    }

    /// ⌘A: every line from the oldest the host keeps to the newest; the history not cached
    /// yet is fetched so the copy that follows has it.
    pub fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        let size = self.state.size();
        let oldest = self.state.scrollback().oldest();
        let total = self.state.scrollback().total();
        let newest = LineIndex(total.saturating_sub(1).max(oldest.0));
        self.selection = Some(Selection::run((oldest, 0), (newest, size.cols.saturating_sub(1))));
        self.selecting = false;
        for (start, count) in self.state.scrollback().missing(oldest, total) {
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            self.send(TermRequest::FetchLines { start, count });
        }
        cx.notify();
    }

    /// ⌘↑ (`delta` −1) / ⌘↓ (+1), also the phone's armed ⌘ with the bar's ↑ / ↓.
    fn step_prompt(&mut self, delta: i8, cx: &mut Context<Self>) {
        let top = self.state.index_at_row(0);
        let target =
            if delta < 0 { self.state.prompt_before(top) } else { self.state.prompt_after(top) };
        match target {
            Some(target) => self.jump_to(target, cx),
            None if delta > 0 => {
                self.state.scroll_to_bottom();
                cx.notify();
            }
            None => {}
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

    /// ⌘V, or the phone key bar's "paste": the clipboard into the session (the host brackets
    /// it when the program asked).
    pub fn paste_clipboard(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        let Some(text) = item.text() else { return };
        self.selection = None;
        self.state.scroll_to_bottom();
        let bracketed = self.state.modes().contains(TermModes::BRACKETED_PASTE);
        if self.theme.behaviour.paste_protection && !paste_is_safe(&text, bracketed) {
            self.pending = Some(Pending::Paste(text));
        } else {
            self.send(TermRequest::Paste(text));
        }
        cx.notify();
    }

    /// What waits goes through (↩, or the bar's first button): the paste is sent, the
    /// close is confirmed to the canvas.
    pub fn confirm_pending(&mut self, cx: &mut Context<Self>) {
        match self.pending.take() {
            Some(Pending::Paste(text)) => self.send(TermRequest::Paste(text)),
            Some(Pending::Close(_)) => cx.emit(TerminalViewEvent::CloseConfirmed),
            None => return,
        }
        cx.notify();
    }

    /// What waits is dropped (Esc, or the Cancel button).
    pub fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        if self.pending.take().is_some() {
            cx.notify();
        }
    }

    /// The paste waiting for a confirmation, when there is one.
    #[must_use]
    pub fn pending_paste(&self) -> Option<&str> {
        match &self.pending {
            Some(Pending::Paste(text)) => Some(text),
            _ => None,
        }
    }

    /// The canvas is about to close this shell: with a command running and the setting
    /// on, the bar asks first and this says so (`true`); otherwise nothing stands in the
    /// way.
    pub fn ask_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.theme.behaviour.confirm_close || !self.state.command_running() {
            return false;
        }
        let command = self
            .state
            .running_command()
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        self.pending = Some(Pending::Close(command));
        cx.notify();
        true
    }

    /// Whether the close of this shell waits on a confirmation.
    #[must_use]
    pub const fn close_asked(&self) -> bool {
        matches!(self.pending, Some(Pending::Close(_)))
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
    pub fn font_family(&self) -> Option<SharedString> {
        self.font_family.clone()
    }

    /// Record the resolved family.
    pub fn set_font_family(&mut self, family: SharedString) {
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

    /// The guesses to draw and the cursor after them, when prediction is showing.
    #[must_use]
    pub fn predictions(&self) -> Option<(&VecDeque<Prediction>, Cursor)> {
        if !self.predictor.visible(Instant::now()) || self.state.view_offset() != 0 {
            return None;
        }
        Some((self.predictor.pending(), self.predictor.cursor(self.state.cursor())))
    }

    /// Whether a typed key still waits to be seen on screen: the element times the frames it
    /// presents only then.
    #[must_use]
    pub fn latency_waiting(&self) -> bool {
        self.latency.waiting()
    }

    /// A frame reached the display at `now`: its guesses showed the keys in `shown` (their
    /// sequence numbers) and its grid the host's state after key `input_ack`.
    pub fn presented(&mut self, now: Instant, shown: &[u64], input_ack: u64) {
        self.latency.painted(now, shown, input_ack);
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

    /// Whether the ⌥ of the key being handled is Alt: the setting, for the side that is down.
    fn alt_is_alt(&self) -> bool {
        self.theme.behaviour.option_as_alt.applies(slopty_platform::right_option_held())
    }

    /// Send a key as if it had been pressed with the terminal focused (key bar buttons).
    pub fn press(&mut self, keystroke: Keystroke, cx: &mut Context<Self>) {
        self.type_key(keystroke, false, cx);
    }

    /// A key typed, first press or auto-repeat (`held`): both go to the predictor, the
    /// latency meter and the bottom of the history, so a held key echoes locally like a tap.
    fn type_key(&mut self, mut keystroke: Keystroke, held: bool, cx: &mut Context<Self>) {
        // An armed ⌘ with the bar's ↑ / ↓ is ⌘↑ / ⌘↓: between prompts, not to the program.
        if self.sticky_command && matches!(keystroke.key.as_str(), "up" | "down") {
            self.sticky_command = false;
            self.step_prompt(if keystroke.key == "up" { -1 } else { 1 }, cx);
            return;
        }
        // An armed ⌘ with the bar's ← → ⌫ is the Mac's line-editing chord, as on the desktop.
        if self.sticky_command
            && self.theme.behaviour.natural_editing
            && let Some(bytes) = keys::natural_editing(&Keystroke {
                modifiers: gpui::Modifiers { platform: true, ..gpui::Modifiers::default() },
                key: keystroke.key.clone(),
                ..Keystroke::default()
            })
        {
            self.sticky_command = false;
            if self.state.view_offset() != 0 {
                self.state.scroll_to_bottom();
            }
            self.send(TermRequest::Raw(bytes.to_vec()));
            cx.notify();
            return;
        }
        if std::mem::take(&mut self.sticky_control) {
            keystroke.modifiers.control = true;
        }
        self.key_seq = self.key_seq.wrapping_add(1);
        let key = keys::key_event(self.key_seq, &keystroke, held, self.alt_is_alt());
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

    /// A key of the phone's bar: [`Self::press`].
    pub fn bar_key(&mut self, keystroke: Keystroke, cx: &mut Context<Self>) {
        self.press(keystroke, cx);
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
            tracing::info!(session = %self.session, epoch = ?self.state.epoch(), "line numbering changed");
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
        if self.state.epoch() != self.took_epoch {
            self.took.clear();
            self.took_epoch = self.state.epoch();
        }
        for effect in effects {
            match effect {
                Effect::Request(req) => self.send(req),
                Effect::Title(t) => cx.emit(TerminalViewEvent::Title(t)),
                Effect::Bell => {
                    self.ring(cx);
                    cx.emit(TerminalViewEvent::Bell);
                }
                Effect::Notification { title, body } => {
                    cx.emit(TerminalViewEvent::Notification { title, body });
                }
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
                Effect::CommandStarted(command) => {
                    tracing::info!(session = %self.session, %command, "command started");
                    self.command_started = Some(Instant::now());
                }
                Effect::CommandFinished { prompt, command, exit } => {
                    let started = self.command_started.take();
                    let elapsed = started.map_or(Duration::ZERO, |t| t.elapsed());
                    tracing::info!(
                        session = %self.session,
                        %command,
                        ?exit,
                        ?elapsed,
                        timed = started.is_some(),
                        "command finished"
                    );
                    if let Some(prompt) = prompt {
                        self.set_took(prompt, elapsed);
                    }
                    cx.emit(TerminalViewEvent::CommandFinished { command, exit, elapsed });
                }
            }
        }
        cx.notify();
    }

    /// A command typed at `prompt` took `elapsed`: kept for the row's caption when it is at
    /// or over `TOOK_MIN` (a quick command says nothing worth a caption).
    pub fn set_took(&mut self, prompt: LineIndex, elapsed: Duration) {
        if elapsed >= TOOK_MIN {
            self.took.insert(prompt, elapsed);
        }
    }

    /// How long the command typed at `prompt` took, when it was long enough to say.
    #[must_use]
    pub fn took(&self, prompt: LineIndex) -> Option<Duration> {
        self.took.get(&prompt).copied()
    }

    /// The images the latest frame places, each with its texture.
    ///
    /// A texture is made once per image generation when the pixels are first painted, and
    /// dropped from the atlas when the state forgets the pixels (its cache budget) or a newer
    /// generation replaces them.
    pub fn placed_images(&mut self, window: &mut Window) -> Vec<PlacedImage> {
        let mut out = Vec::new();
        for placement in self.state.placements() {
            let Some(pixels) = self.state.image(placement) else { continue };
            let texture = match self.textures.get(&placement.image) {
                Some(t) if t.generation == placement.generation => Arc::clone(&t.image),
                _ => {
                    let Some(image) = texture_of(pixels) else { continue };
                    let made =
                        Texture { generation: placement.generation, image: Arc::clone(&image) };
                    if let Some(old) = self.textures.insert(placement.image, made) {
                        let _dropped = window.drop_image(old.image);
                    }
                    image
                }
            };
            let (width, height) = (pixels.width, pixels.height);
            out.push(PlacedImage { placement: *placement, image: texture, width, height });
        }
        let stale: Vec<u32> = self
            .textures
            .iter()
            .filter(|(id, t)| !self.state.holds(**id, t.generation))
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(gone) = self.textures.remove(&id) {
                let _dropped = window.drop_image(gone.image);
            }
        }
        out
    }

    /// Textures currently made (tests).
    #[must_use]
    pub fn texture_count(&self) -> usize {
        self.textures.len()
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

    /// The blink clock's phase: true while a blinking cursor or SGR 5 text shows.
    #[must_use]
    pub const fn blink_on(&self) -> bool {
        self.blink_on
    }

    /// The element painted a frame: `wanted` says whether it held anything that blinks. The
    /// clock starts on the first such frame and stops after the first frame without.
    pub fn blinking(&mut self, wanted: bool, cx: &Context<Self>) {
        self.blink_wanted = wanted;
        if !wanted || self.blink_task.is_some() {
            return;
        }
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_HALF).await;
                let more = this.update(cx, Self::blink_tick).unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }

    /// Half a blink passed: flip the phase, unless a keystroke pinned it on less than a half
    /// ago (typing keeps the cursor solid). False ends the loop, phase on.
    fn blink_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.blink_wanted {
            self.blink_task = None;
            if !self.blink_on {
                self.blink_on = true;
                cx.notify();
            }
            return false;
        }
        if self.blink_pinned.take().is_some_and(|at| at.elapsed() < BLINK_HALF) {
            return true;
        }
        self.blink_on = !self.blink_on;
        cx.notify();
        true
    }

    /// Whether the grid is tinted for a bell right now.
    #[must_use]
    pub const fn bell_flashing(&self) -> bool {
        self.bell_flash
    }

    /// A bell: tint the grid for [`BELL_FLASH`]; what the platform does about it (a sound, the
    /// Dock) is the app's call.
    fn ring(&mut self, cx: &mut Context<Self>) {
        self.bell_flash = true;
        cx.notify();
        self.bell_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BELL_FLASH).await;
            // A gone view has nothing to un-tint.
            let _gone = this.update(cx, |view, cx| {
                view.bell_flash = false;
                view.bell_task = None;
                cx.notify();
            });
        }));
    }

    /// A keystroke: the cursor shows solid for a full half-blink from now.
    fn pin_blink(&mut self) {
        self.blink_on = true;
        self.blink_pinned = Some(Instant::now());
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
        // Esc closes the block menu and goes no further.
        if self.block_menu.is_some() && event.keystroke.key == "escape" {
            self.block_menu = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // Something waiting at the foot (a held-back paste, a close): ↩ confirms it, Esc
        // drops it, any other key drops it and goes on to the program as typed (nothing is
        // swallowed).
        if self.pending.is_some() && !event.keystroke.modifiers.platform {
            match event.keystroke.key.as_str() {
                "enter" => {
                    self.confirm_pending(cx);
                    cx.stop_propagation();
                    return;
                }
                "escape" => {
                    self.cancel_pending(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => self.cancel_pending(cx),
            }
        }
        // Typing in the search field must never reach the program.
        if self
            .search
            .as_ref()
            .is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window))
        {
            return;
        }
        // The Mac's line-editing chords, sent as readline's bytes (ghostty's macOS "natural
        // text editing" keybinds).
        if self.theme.behaviour.natural_editing
            && let Some(bytes) = keys::natural_editing(&event.keystroke)
        {
            self.selection = None;
            self.pin_blink();
            if self.state.view_offset() != 0 {
                self.state.scroll_to_bottom();
            }
            self.send(TermRequest::Raw(bytes.to_vec()));
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Cmd shortcuts belong to the app.
        if event.keystroke.modifiers.platform {
            tracing::debug!(session = %self.session, key = %event.keystroke.key, "cmd key passed up");
            return;
        }
        // ⇧-arrows move a selection's head (ghostty's `adjust_selection`); with nothing
        // selected they are the program's, as every other key.
        if let Some(selection) = self.selection
            && let Some(head) = self.adjusted_head(selection, &event.keystroke)
        {
            self.selection = Some(Selection { head, ..selection });
            cx.stop_propagation();
            cx.notify();
            return;
        }
        self.selection = None;
        self.pin_blink();
        if self.theme.behaviour.hide_pointer_while_typing {
            slopty_platform::hide_pointer_until_moved();
        }
        self.type_key(event.keystroke.clone(), event.is_held, cx);
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
        let lines = lines * f32::from(self.theme.behaviour.scroll_multiplier) / 100.0;
        // ⌘-wheel is the canvas's zoom, never the grid's.
        if event.modifiers.platform {
            self.wheel_remainder = 0.0;
            self.wheel_gesture = None;
            return;
        }
        // A program that asked for the mouse gets the wheel (⇧ keeps it for scrolling, as
        // in every terminal); so does anything on the alternate screen, which has no
        // history here to scroll — the host turns it into cursor keys (alternate scroll).
        let modes = self.state.modes();
        let to_program = (modes.contains(TermModes::MOUSE_TRACKING) && !event.modifiers.shift)
            || modes.contains(TermModes::ALT_SCREEN);
        // The grid can take the wheel while it has somewhere to go with it — a program wants
        // it, or there is history that way — and otherwise lets it through, so the canvas pans
        // under a grid at the end of its history rather than swallowing the gesture.
        let can_use = to_program
            || (lines > 0.0 && self.state.view_offset() < self.state.history_len())
            || (lines < 0.0 && self.state.view_offset() > 0);
        // A gesture goes to whichever surface could use its first *movement* and keeps it to
        // the last. Deciding per event instead would hand a fling's momentum to the canvas the
        // moment the grid ran out of scrollback, and the whole workspace would slide out from
        // under a terminal that was merely flicked too hard.
        //
        // The latch outlives `Ended` on purpose. gpui reads only `NSEvent.phase`, never
        // `momentumPhase`, so macOS momentum arrives as a run of `Moved` *after* the fingers
        // lift — releasing on `Ended` would drop the latch at exactly the moment it is needed.
        // Only `Started` clears it, and `Started` itself decides nothing: the first event of a
        // gesture is a finger landing, and it carries no movement to judge.
        //
        // Only a gesture latches. `hasPreciseScrollingDeltas` is what picks `Pixels` over
        // `Lines`, so on macOS a `Lines` delta is a mouse wheel and nothing else: a notch has no
        // gesture to be part of and is judged on its own, rather than inheriting whatever the
        // last trackpad fling decided.
        if event.touch_phase == TouchPhase::Started {
            // A new gesture also counts its fractions of a line from zero.
            self.wheel_remainder = 0.0;
            self.wheel_gesture = None;
        }
        let mine = match (event.delta, self.wheel_gesture) {
            (ScrollDelta::Lines(_), _) => can_use,
            (ScrollDelta::Pixels(_), Some(mine)) => mine,
            // A program that wants the mouse owns the gesture whichever way it goes, so there
            // is nothing to wait to see; otherwise the direction is the whole question.
            (ScrollDelta::Pixels(_), None) if to_program || lines.abs() > f32::EPSILON => {
                self.wheel_gesture = Some(can_use);
                can_use
            }
            // Still nothing to scroll and nothing to decide: leave the gesture unowned.
            (ScrollDelta::Pixels(_), None) => return,
        };
        if !mine {
            self.wheel_remainder = 0.0;
            return;
        }
        cx.stop_propagation();
        // A trackpad moves in fractions of a line: they add up to whole ones, and a new
        // gesture starts the count over.
        let total = self.wheel_remainder + lines;
        // Wheel up (positive y in GPUI) scrolls into history.
        #[expect(clippy::cast_possible_truncation, reason = "whole lines")]
        let delta = total.trunc() as i64;
        #[expect(clippy::cast_precision_loss, reason = "the truncated part of an f32")]
        let remainder = total - delta as f32;
        self.wheel_remainder = remainder;
        if delta == 0 {
            return;
        }
        if to_program {
            let Some(m) = self.metrics else { return };
            let (col, row) = m.cell_at_clamped(event.position);
            let (px, py) = m.pixel_at(event.position);
            let rows =
                i16::try_from(delta.clamp(i64::from(i16::MIN), i64::from(i16::MAX))).unwrap_or(0);
            self.send(TermRequest::Mouse(MouseEvent {
                action: MouseAction::Wheel { rows, cols: 0 },
                button: None,
                mods: keys::mods(event.modifiers),
                col,
                row,
                px,
                py,
            }));
            return;
        }
        self.scroll_lines(delta, cx);
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        if self.block_menu.take().is_some() {
            cx.notify();
        }
        // The scrollbar, when it shows, takes the clicks over it: the thumb is dragged, the
        // track beside it pages towards the click.
        if event.button == MouseButton::Left
            && let Some(thumb) = self.thumb()
        {
            if thumb.contains(&event.position) {
                self.thumb_drag = Some(event.position.y - thumb.origin.y);
                cx.notify();
                return;
            }
            let track_x = event.position.x >= thumb.origin.x
                && event.position.x < thumb.origin.x + thumb.size.width;
            if track_x && let Some(m) = self.metrics {
                let page = i64::from(m.rows).max(1);
                let up = event.position.y < thumb.origin.y;
                self.scroll_lines(if up { page } else { page.saturating_neg() }, cx);
                return;
            }
        }
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
            } else if let Some(span) =
                self.state.line(index).and_then(|line| url::path_at_col(line, col))
            {
                self.path_press = Some((span, armed, event.position));
            } else if armed {
                // The phone has no right button: the armed tap on a bare row is its menu.
                let block = self.state.command_block(index);
                self.block_menu = Some(BlockMenu { block, at: event.position });
                cx.notify();
            }
            return;
        }
        // Left button selects unless the program asked for the mouse (⇧ overrides, as in
        // every terminal); everything else is reported to the program.
        let program_wants_mouse = self.state.modes().contains(TermModes::MOUSE_TRACKING);
        // Right button opens the menu: the command block's items when the row is in one
        // (shell integration marks them), the terminal's own always.
        if event.button == MouseButton::Right && (!program_wants_mouse || event.modifiers.shift) {
            let block = self.state.command_block(self.state.index_at_row(row));
            self.block_menu = Some(BlockMenu { block, at: event.position });
            cx.notify();
            return;
        }
        if event.button == MouseButton::Left && (!program_wants_mouse || event.modifiers.shift) {
            let index = self.state.index_at_row(row);
            self.selected_by_press = true;
            if event.click_count >= 2 {
                // Word, then line; the selection stands until the next click.
                self.select_by_clicks(index, col, event.click_count);
                self.selecting = false;
            } else if event.modifiers.shift
                && let Some(selection) = &mut self.selection
            {
                // ⇧-click moves the near end of the selection, as in every terminal.
                selection.head = (index, col);
                self.selecting = true;
            } else {
                // ⌥-drag selects a rectangle, as in every terminal.
                let at = (index, col);
                self.selection =
                    Some(Selection { anchor: at, head: at, block: event.modifiers.alt });
                self.selecting = true;
                self.click_at = (!event.modifiers.modified()).then_some(at);
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

    /// The pointer moved over the grid: the hover for the ⌘ underline, and the scrollbar
    /// shows itself while the pointer is over the card (a drag is followed by
    /// [`Self::drag_move`], which the element registers on the window so the drag can leave
    /// the card).
    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.scrollbar_shown();
        let hover = self.metrics.and_then(|m| m.cell_at(event.position));
        self.set_pointer(hover, event.modifiers, cx);
        if shown != self.scrollbar_shown() {
            cx.notify();
        }
    }

    /// A drag with the left button, wherever the pointer is: the thumb held scrolls the
    /// viewport with it; a selection follows the pointer, and past the grid's top or bottom
    /// it keeps scrolling (`AUTOSCROLL_TICK`) at a pace set by how far past the pointer is.
    pub(super) fn drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left)
            && let Some((_, _, from)) = &self.path_press
            && (event.position - *from).magnitude() >= f64::from(DRAG_OUT_SLOP)
            && let Some((span, ..)) = self.path_press.take()
        {
            tracing::info!(path = %span.path, "drag out");
            cx.emit(TerminalViewEvent::DragOut { path: span.path });
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            self.thumb_drag = None;
            self.autoscroll = None;
            return;
        }
        let Some(m) = self.metrics else { return };
        if let Some(grab) = self.thumb_drag {
            let history = self.state.history_len();
            let offset = super::element::offset_for_thumb(&m, history, event.position.y - grab);
            if offset != self.state.view_offset() {
                for effect in self.state.scroll_to(offset) {
                    if let Effect::Request(req) = effect {
                        self.send(req);
                    }
                }
                cx.notify();
            }
            return;
        }
        if !self.selecting {
            return;
        }
        let (col, row) = m.cell_at_clamped(event.position);
        let past = super::element::rows_past_edge(&m, event.position.y);
        if past == 0 {
            self.autoscroll = None;
        } else {
            self.start_autoscroll(past.clamp(-AUTOSCROLL_MAX, AUTOSCROLL_MAX), col, cx);
        }
        let head = (self.state.index_at_row(row), col);
        if let Some(selection) = &mut self.selection
            && selection.head != head
        {
            selection.head = head;
            cx.notify();
        }
    }

    /// Keep scrolling `lines` a tick (positive = up) while the drag stays past the edge.
    fn start_autoscroll(&mut self, lines: i64, col: u16, cx: &Context<Self>) {
        let running = self.autoscroll.is_some();
        self.autoscroll = Some((lines, col));
        if running {
            return;
        }
        self.autoscroll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_TICK).await;
                let more = this.update(cx, Self::autoscroll_tick).unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }

    /// One tick of the drag past the edge: scroll, and put the selection's head on the row
    /// that came into view. False ends the loop.
    fn autoscroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((lines, col)) = self.autoscroll else { return false };
        if !self.selecting {
            self.autoscroll = None;
            return false;
        }
        self.scroll_lines(lines, cx);
        let row = if lines > 0 { 0 } else { self.state.size().rows.saturating_sub(1) };
        let head = (self.state.index_at_row(row), col);
        if let Some(selection) = &mut self.selection {
            selection.head = head;
        }
        cx.notify();
        true
    }

    /// Scroll the viewport by `lines` (positive = up into history), fetching what is not
    /// cached.
    fn scroll_lines(&mut self, lines: i64, cx: &mut Context<Self>) {
        for effect in self.state.scroll(lines) {
            if let Effect::Request(req) = effect {
                self.send(req);
            }
        }
        cx.notify();
    }

    /// The scrollbar's thumb, when the bar shows.
    fn thumb(&self) -> Option<Bounds<Pixels>> {
        if !self.scrollbar_shown() {
            return None;
        }
        let m = self.metrics?;
        super::element::scrollbar_thumb(&m, self.state.history_len(), self.state.view_offset())
    }

    /// The thumb is held by the pointer (drawn stronger).
    pub(super) const fn thumb_held(&self) -> bool {
        self.thumb_drag.is_some()
    }

    /// Whether the scrollbar is drawn: there is history, and the viewport is in it, or the
    /// pointer is over the grid, or the thumb is held.
    pub(super) const fn scrollbar_shown(&self) -> bool {
        self.state.history_len() > 0
            && (self.state.view_offset() > 0 || self.hover.is_some() || self.thumb_drag.is_some())
    }

    fn mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.autoscroll = None;
        if let Some((span, armed, _)) = self.path_press.take() {
            self.open_path(&span, armed, cx);
            return;
        }
        if self.thumb_drag.take().is_some() {
            cx.notify();
            return;
        }
        let selecting = std::mem::take(&mut self.selecting);
        let by_press = std::mem::take(&mut self.selected_by_press);
        if !selecting && !by_press {
            return;
        }
        // A click without a drag selects nothing; a plain one moves the shell's cursor.
        let click_at = self.click_at.take();
        if self.selection.is_some_and(|s| s.anchor == s.head) {
            self.selection = None;
            if let Some((index, col)) = click_at {
                self.click_to_move(index, col, cx);
            }
        }
        if by_press {
            self.copy_on_select(cx);
        }
        cx.notify();
    }

    /// A click on the shell's input line puts the cursor there, with the arrow keys that get
    /// it there (ghostty's `cursor-click-to-move`); anywhere else it does nothing.
    fn click_to_move(&mut self, index: LineIndex, col: u16, cx: &mut Context<Self>) {
        let Some((rows, cells)) = self.state.cursor_path_to(index, col) else { return };
        let arrows = |key: &'static str, n: i32| {
            std::iter::repeat_n(key, usize::try_from(n.unsigned_abs()).unwrap_or(0))
        };
        let keys = arrows(if rows < 0 { "up" } else { "down" }, rows)
            .chain(arrows(if cells < 0 { "left" } else { "right" }, cells));
        // The phone's armed ⌃ is for the next typed key, not for these.
        let sticky_control = std::mem::take(&mut self.sticky_control);
        for key in keys {
            self.press(Keystroke { key: key.to_owned(), ..Keystroke::default() }, cx);
        }
        self.sticky_control = sticky_control;
    }

    /// A selection just made goes to the clipboard when the theme asks for it.
    fn copy_on_select(&self, cx: &Context<Self>) {
        if self.theme.behaviour.copy_on_select
            && let Some(text) = self.selected_text().filter(|t| !t.is_empty())
        {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }
}

/// Text input on top of the key path.
///
/// Keys reach the terminal through `TerminalView::key_down`; this handler makes the platform
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
        let wash = hsla_alpha(s.text, alpha::FAINT);
        let bare = move |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
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
        } else if search.total == 0 {
            "none".into()
        } else {
            let at = search.current.map_or(0, |c| c.saturating_add(1));
            let more = if search.total > SEARCH_MAX { "+" } else { "" };
            format!("{at}/{}{more}", search.total).into()
        };
        div()
            .id("terminal-search")
            .debug_selector(|| "terminal-search".to_owned())
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
            .child(
                div()
                    .id("terminal-search-count")
                    .min_w(px(40.0))
                    .text_color(hsla(s.text_secondary))
                    .role(gpui::accesskit::Role::Label)
                    .aria_label("Matches")
                    .aria_value(count.clone())
                    .child(count),
            )
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
        let search_focused = self
            .search
            .as_ref()
            .is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window));
        let search = self.search.as_ref().map(|s| self.render_search(s, search_focused, cx));
        let header = self.block_header().and_then(|block| self.render_block_header(&block, cx));
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
            .on_action(cx.listener(Self::rerun_last))
            .on_action(cx.listener(Self::note_last_block))
            .on_action(cx.listener(Self::clear_screen))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .on_action(cx.listener(Self::scroll_to_top))
            .on_action(cx.listener(Self::scroll_to_bottom))
            .on_action(cx.listener(Self::select_all))
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .map(|el| {
                let el = el.cursor(self.pointer());
                let mut grid =
                    TerminalElement::new(cx.entity(), focused).zoom(self.zoom).zooming(zooming);
                if window.is_a11y_active() {
                    let label = self.title().unwrap_or("shell").to_owned();
                    grid = grid.a11y(label.into(), self.cursor_row_text().into());
                }
                el.child(grid)
            })
            .children(header)
            .children(search)
            .children(self.render_link_preview())
            .children(self.render_confirm(cx))
            .children(self.block_menu.as_ref().map(|m| self.render_block_menu(m, cx)))
    }
}

/// The command-block menu a right click opened.
struct BlockMenu {
    /// The block under the click; `None` on a row outside every block (the terminal's own
    /// items only).
    block: Option<CommandBlock>,
    /// Where the click landed (window coordinates), the menu's anchor.
    at: gpui::Point<Pixels>,
}

/// What waits on a confirmation at the card's foot.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Pending {
    /// A paste held back by paste protection until ↩ or the Paste button sends it.
    Paste(String),
    /// The close of a shell whose command (the first line of it) is still running.
    Close(String),
}

/// What the right-click menu offers: a block's items on a block, the terminal's own after.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockMenuItem {
    /// The typed command to the clipboard.
    CopyCommand,
    /// The command's output to the clipboard.
    CopyOutput,
    /// Type the command again and press ↩.
    Rerun,
    /// The block as a note card beside the shell: the command runnable, the output under it.
    Note,
    /// Select the whole block, prompt to last output row.
    SelectBlock,
    /// The selection to the clipboard.
    Copy,
    /// The clipboard into the session.
    Paste,
    /// The find field.
    Find,
    /// Clear the screen (`clear`, or the sequence, as ⌘K does).
    ClearScreen,
}

/// The shortest command whose row gets a "took" caption.
pub const TOOK_MIN: Duration = Duration::from_secs(1);

/// A command's duration for its row's caption: `1.4 s` under a minute, `2 m 03 s` under an
/// hour, `1 h 02 m` from there.
#[must_use]
pub fn took_label(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{:.1} s", elapsed.as_secs_f64())
    } else if secs < 3600 {
        format!("{} m {:02} s", secs / 60, secs % 60)
    } else {
        format!("{} h {:02} m", secs / 3600, (secs % 3600) / 60)
    }
}

/// A command block as a note: the command as a heading and a runnable `sh` fence, the
/// output as a plain fence under it; either half alone when the block has only that.
#[must_use]
pub fn block_note(block: &CommandBlock) -> String {
    let mut text = String::new();
    if let Some(command) = &block.command {
        text.push_str("# ");
        text.push_str(command.lines().next().unwrap_or_default());
        text.push_str("\n\n```sh\n");
        text.push_str(command);
        text.push_str("\n```\n");
    }
    if !block.output.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("```\n");
        text.push_str(&block.output);
        text.push_str("\n```\n");
    }
    text
}

impl BlockMenuItem {
    const fn key(self) -> &'static str {
        match self {
            Self::CopyCommand => "copy-command",
            Self::CopyOutput => "copy-output",
            Self::Rerun => "rerun",
            Self::Note => "note",
            Self::SelectBlock => "select-block",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::Find => "find",
            Self::ClearScreen => "clear",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::CopyCommand => "Copy command",
            Self::CopyOutput => "Copy output",
            Self::Rerun => "Rerun",
            Self::Note => "Save as note",
            Self::SelectBlock => "Select block",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::Find => "Find…",
            Self::ClearScreen => "Clear screen",
        }
    }
}

/// A GPUI texture of the pixels: BGRA with the alpha premultiplied, as the renderer samples.
fn texture_of(pixels: &TermImage) -> Option<Arc<gpui::RenderImage>> {
    let bgra: Vec<u8> = pixels
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let pre = |c: u8| {
                u8::try_from(u32::from(c).saturating_mul(u32::from(a)) / 255).unwrap_or(255)
            };
            [pre(b), pre(g), pre(r), a]
        })
        .collect();
    let buffer = image::RgbaImage::from_raw(pixels.width, pixels.height, bgra)?;
    Some(Arc::new(gpui::RenderImage::new([image::Frame::new(buffer)])))
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Pixels, TestAppContext, VisualTestContext, px, size};
    use slopty_grid::{Hyperlink, Line, RowUpdate, SemanticMark, Style, TermModes};
    use slopty_proto::terminal::{Frame, TermRequest};

    use super::*;

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

    fn marked(text: &str, mark: SemanticMark) -> Line {
        let mut line = Line::from_text(text, 10, Style::DEFAULT);
        line.mark = mark;
        line
    }

    /// Three command blocks: `ls` (a, b), `false` (nothing), `seq 2` (1, 2, blank), then the
    /// newest prompt. Lines 0..=5 are history the host already sent, 6..=8 the screen.
    fn with_command_blocks(view: &Entity<TerminalView>, cx: &mut VisualTestContext) {
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
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
                    images: Vec::new(),
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
        assert_eq!(failed, hsla_alpha(theme.surfaces.error, alpha::STRONG));
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

    /// A block whose prompt rows have scrolled above the viewport keeps its command in a
    /// sticky header over the top row; on a prompt row there is none, and a click on the
    /// header brings the prompt back to the top.
    #[gpui::test]
    fn a_block_scrolled_past_its_prompt_keeps_its_command_in_a_sticky_header(
        cx: &mut TestAppContext,
    ) {
        let (view, _rx, cx) = terminal(cx);
        with_command_blocks(&view, cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();

        assert_eq!(top_line(&view, cx), LineIndex(6), "inside `seq 2`'s output");
        let header = cx.debug_bounds("block-header").expect("the header is drawn");
        let terminal = cx.debug_bounds("terminal").expect("the terminal is drawn");
        let line_height = view.read_with(cx, |view, _| view.metrics.expect("laid out").line_height);
        assert_eq!(header.origin, terminal.origin, "over the top row");
        assert_eq!(header.size.width, terminal.size.width);
        assert_eq!(header.size.height, line_height, "one row high");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(
            tree.iter().any(|n| n.role == "Button" && n.label.as_deref() == Some("seq 2")),
            "the header names the command: {tree:?}"
        );
        let command = view.read_with(cx, |view, _| view.block_header().and_then(|b| b.command));
        assert_eq!(command.as_deref(), Some("seq 2"));
        // The block's duration, once known, sits at the header's right end.
        assert!(cx.debug_bounds("block-header-took").is_none(), "nothing known yet");
        view.update(cx, |v, cx| {
            v.set_took(LineIndex(4), Duration::from_millis(3_260));
            cx.notify();
        });
        cx.run_until_parked();
        let took = cx.debug_bounds("block-header-took").expect("the header says how long");
        assert!(took.right() <= header.right() && took.left() > header.center().x, "{took:?}");

        cx.simulate_keystrokes("cmd-up");
        assert_eq!(top_line(&view, cx), LineIndex(4), "`$ seq 2` at the top");
        assert!(cx.debug_bounds("block-header").is_none(), "the prompt itself is visible");

        cx.simulate_keystrokes("cmd-down");
        assert_eq!(top_line(&view, cx), LineIndex(6));
        assert!(cx.debug_bounds("block-header").is_some(), "back inside the output");
        cx.simulate_click(header.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(top_line(&view, cx), LineIndex(4), "a click scrolls to the prompt");
        assert!(cx.debug_bounds("block-header").is_none());
    }

    /// A placed image gets one texture, kept across frames while its generation holds and
    /// dropped when the frame stops placing it and the state forgets the pixels.
    #[gpui::test]
    fn a_placed_image_has_one_texture_until_its_pixels_are_forgotten(cx: &mut TestAppContext) {
        use slopty_proto::terminal::PixelRect;
        let (view, _rx, cx) = terminal(cx);
        let placement = Placement {
            image: 7,
            generation: 3,
            col: 0,
            row: 0,
            cols: 1,
            rows: 1,
            x_offset: 0,
            y_offset: 0,
            width: 2,
            height: 1,
            source: PixelRect { x: 0, y: 0, width: 2, height: 1 },
            z: 0,
        };
        let frame = |seq, images: Vec<Placement>| {
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
                images,
                updates: Vec::new(),
            })
        };
        let image = TermEvent::Image {
            id: 7,
            generation: 3,
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 128],
        };
        view.update_in(cx, |view, _window, cx| {
            view.apply(image, cx);
            view.apply(frame(1, vec![placement]), cx);
        });
        let placed = view.update_in(cx, |view, window, _cx| view.placed_images(window));
        assert_eq!(placed.len(), 1);
        assert_eq!((placed[0].width, placed[0].height), (2, 1));
        assert_eq!(placed[0].placement, placement);
        let first = Arc::clone(&placed[0].image);
        // Half-transparent blue premultiplied to BGRA.
        assert_eq!(first.as_bytes(0), Some(&[0, 0, 255, 255, 128, 0, 0, 128][..]));
        // The next frame places it again: the same texture.
        view.update_in(cx, |view, _window, cx| view.apply(frame(2, vec![placement]), cx));
        let again = view.update_in(cx, |view, window, _cx| view.placed_images(window));
        assert!(Arc::ptr_eq(&again[0].image, &first), "made once");
        assert_eq!(view.read_with(cx, |view, _cx| view.texture_count()), 1);
        // A frame without it keeps the texture while the pixels are held (a scroll may bring
        // it back); a newer generation of the id replaces it.
        view.update_in(cx, |view, _window, cx| view.apply(frame(3, Vec::new()), cx));
        let _none = view.update_in(cx, |view, window, _cx| view.placed_images(window));
        assert_eq!(view.read_with(cx, |view, _cx| view.texture_count()), 1);
        let newer = Placement { generation: 4, ..placement };
        view.update_in(cx, |view, _window, cx| {
            view.apply(
                TermEvent::Image { id: 7, generation: 4, width: 1, height: 1, rgba: vec![9; 4] },
                cx,
            );
            view.apply(frame(4, vec![newer]), cx);
        });
        let replaced = view.update_in(cx, |view, window, _cx| view.placed_images(window));
        assert!(!Arc::ptr_eq(&replaced[0].image, &first), "a new generation, a new texture");
        assert_eq!(view.read_with(cx, |view, _cx| view.texture_count()), 1);
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
                images: Vec::new(),
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

    /// The blink clock runs only while a painted frame blinks: a frame with SGR 5 text (or a
    /// blinking cursor, while focused) starts it, each half-blink flips the phase, a keystroke
    /// pins the phase on for a half, and a frame with nothing to blink stops it, phase on.
    /// BEL tints the grid for a flash and then leaves it alone; a second bell inside the
    /// flash restarts it rather than ending it early.
    /// ⌘ over a printed URL previews it; over an OSC 8 label, the target the label hides;
    /// over a path, the path with its line. The chip sits in the card while it applies.
    #[gpui::test]
    fn a_cmd_hover_previews_the_links_target(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let row = |row, text: &str, links| RowUpdate {
            row,
            line: {
                let mut l = Line::from_text(text, 30, Style::DEFAULT);
                l.links = links;
                l
            },
        };
        view.update_in(cx, |view, _window, cx| {
            view.apply(
                TermEvent::Frame(Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols: 30,
                    rows: 3,
                    cursor: Cursor::default(),
                    modes: TermModes::empty(),
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    images: Vec::new(),
                    updates: vec![
                        row(0, "see http://a.b", Vec::new()),
                        row(
                            1,
                            "docs",
                            vec![Hyperlink { col: 0, len: 4, uri: "https://x.y/z".into() }],
                        ),
                        row(2, "at src/main.rs:12", Vec::new()),
                    ],
                }),
                cx,
            );
        });
        cx.run_until_parked();
        let cmd = gpui::Modifiers { platform: true, ..gpui::Modifiers::default() };
        let plain = gpui::Modifiers::default();
        view.update_in(cx, |view, _window, cx| {
            view.set_pointer(Some((6, 0)), cmd, cx);
            assert_eq!(view.link_target().as_deref(), Some("http://a.b"));
            view.set_pointer(Some((1, 1)), cmd, cx);
            assert_eq!(view.link_target().as_deref(), Some("https://x.y/z"), "the OSC 8 target");
            view.set_pointer(Some((5, 2)), cmd, cx);
            assert_eq!(view.link_target().as_deref(), Some("src/main.rs:12"));
            view.set_pointer(Some((5, 2)), plain, cx);
            assert_eq!(view.link_target(), None, "no ⌘, no preview");
        });
        cx.simulate_mouse_move(cell_center(&view, cx, 1.0, 1.0), MouseButton::Left, cmd);
        cx.run_until_parked();
        let chip = cx.debug_bounds("link-preview").expect("the chip is drawn");
        let card = cx.debug_bounds("terminal").expect("the card");
        assert!(chip.bottom() <= card.bottom() && chip.left() >= card.left(), "inside the card");
        cx.simulate_mouse_move(cell_center(&view, cx, 1.0, 1.0), MouseButton::Left, plain);
        cx.run_until_parked();
        assert!(cx.debug_bounds("link-preview").is_none(), "gone with ⌘");
    }

    /// Text gets an I-beam; the link under a ⌘-hover a hand; a program that reports the
    /// mouse an arrow, unless ⇧ takes the pointer back.
    #[gpui::test]
    fn the_pointer_is_an_i_beam_a_hand_over_a_link_and_an_arrow_for_a_program(
        cx: &mut TestAppContext,
    ) {
        let (view, _rx, cx) = terminal(cx);
        let frame = |modes| match history_frame(0, &["see http://a.b", "plain"]) {
            TermEvent::Frame(mut f) => {
                f.modes = modes;
                TermEvent::Frame(f)
            }
            other => other,
        };
        let plain = gpui::Modifiers::default();
        let cmd = gpui::Modifiers { platform: true, ..gpui::Modifiers::default() };
        let shift = gpui::Modifiers { shift: true, ..gpui::Modifiers::default() };
        view.update_in(cx, |view, _window, cx| {
            view.apply(frame(TermModes::empty()), cx);
            view.set_pointer(Some((6, 0)), plain, cx);
            assert_eq!(view.pointer(), CursorStyle::IBeam);
            view.set_pointer(Some((6, 0)), cmd, cx);
            assert_eq!(view.pointer(), CursorStyle::PointingHand, "⌘ over the link");
            view.set_pointer(Some((1, 1)), cmd, cx);
            assert_eq!(view.pointer(), CursorStyle::IBeam, "⌘ over plain text");
            view.apply(frame(TermModes::MOUSE_TRACKING), cx);
            view.set_pointer(Some((1, 1)), plain, cx);
            assert_eq!(view.pointer(), CursorStyle::Arrow, "the program has the mouse");
            view.set_pointer(Some((1, 1)), shift, cx);
            assert_eq!(view.pointer(), CursorStyle::IBeam, "⇧ takes it back");
            view.set_pointer(Some((6, 0)), cmd, cx);
            assert_eq!(view.pointer(), CursorStyle::PointingHand, "a link still opens");
        });
    }

    #[gpui::test]
    fn a_bell_flashes_the_view_briefly(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let flashing = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.bell_flashing());
        assert!(!flashing(cx));
        view.update_in(cx, |view, _window, cx| view.apply(TermEvent::Bell, cx));
        assert!(flashing(cx), "rings: the view flashes");
        cx.background_executor.advance_clock(BELL_HALF);
        cx.run_until_parked();
        view.update_in(cx, |view, _window, cx| view.apply(TermEvent::Bell, cx));
        cx.background_executor.advance_clock(BELL_HALF);
        cx.run_until_parked();
        assert!(flashing(cx), "a second bell restarted the flash");
        cx.background_executor.advance_clock(BELL_HALF);
        cx.run_until_parked();
        assert!(!flashing(cx), "and it is itself again");
    }

    #[gpui::test]
    fn the_blink_clock_ticks_only_while_something_blinks(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let frame = |seq, blink: bool, cursor_blink: bool| {
            let mut style = Style::DEFAULT;
            style.flags.set(slopty_grid::StyleFlags::BLINK, blink);
            TermEvent::Frame(Frame {
                seq,
                full: true,
                epoch: 0,
                cols: 10,
                rows: 3,
                cursor: Cursor { blink: cursor_blink, visible: true, ..Cursor::default() },
                modes: TermModes::empty(),
                oldest_line: LineIndex(0),
                first_visible_line: LineIndex(0),
                total_lines: 3,
                input_ack: 0,
                images: Vec::new(),
                updates: vec![RowUpdate { row: 0, line: Line::from_text("hi", 10, style) }],
            })
        };
        let phase = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| (v.blink_on, v.blink_task.is_some()))
        };
        assert_eq!(phase(cx), (true, false), "a steady frame: no clock");

        view.update_in(cx, |view, _window, cx| view.apply(frame(1, true, false), cx));
        cx.run_until_parked();
        assert_eq!(phase(cx), (true, true), "SGR 5 text starts the clock, shown first");
        cx.background_executor.advance_clock(BLINK_HALF);
        cx.run_until_parked();
        assert_eq!(phase(cx), (false, true), "half a blink later: hidden");
        cx.simulate_keystrokes("a");
        assert!(phase(cx).0, "a keystroke shows it again");
        cx.background_executor.advance_clock(BLINK_HALF);
        cx.run_until_parked();
        assert!(phase(cx).0, "and keeps it shown through the next half");
        cx.background_executor.advance_clock(BLINK_HALF);
        cx.run_until_parked();
        assert_eq!(phase(cx), (false, true), "then it blinks on");

        view.update_in(cx, |view, _window, cx| view.apply(frame(2, false, true), cx));
        cx.run_until_parked();
        cx.background_executor.advance_clock(BLINK_HALF);
        cx.run_until_parked();
        assert!(phase(cx).1, "a blinking cursor keeps the clock while focused");

        view.update_in(cx, |view, _window, cx| view.apply(frame(3, false, false), cx));
        cx.run_until_parked();
        cx.background_executor.advance_clock(BLINK_HALF);
        cx.run_until_parked();
        assert_eq!(phase(cx), (true, false), "nothing blinks: the clock stops, phase on");

        // The theme overrides the program either way.
        let blink_theme = |cursor_blink| {
            let mut theme = Theme::new(slopty_theme::Variant::Dark);
            theme.behaviour.cursor_blink = cursor_blink;
            theme
        };
        view.update_in(cx, |view, _window, cx| {
            view.set_theme(blink_theme(slopty_theme::CursorBlink::Never), cx);
            view.apply(frame(4, false, true), cx);
        });
        cx.run_until_parked();
        assert_eq!(phase(cx), (true, false), "never: a program's blink is steady");
        view.update_in(cx, |view, _window, cx| {
            view.set_theme(blink_theme(slopty_theme::CursorBlink::Always), cx);
            view.apply(frame(5, false, false), cx);
        });
        cx.run_until_parked();
        assert!(phase(cx).1, "always: a steady program's cursor blinks");
    }

    /// Paste protection: a newline into a shell without bracketed paste waits (↩ sends it,
    /// Esc drops it, another key drops it and types), a bracketed paste goes straight
    /// unless it holds the bracket's end, and the setting turns the wait off.
    #[gpui::test]
    fn a_paste_that_would_run_waits_for_a_confirmation(cx: &mut TestAppContext) {
        assert!(paste_is_safe("ls\n", true) && !paste_is_safe("ls\n", false));
        assert!(paste_is_safe("ls", false) && !paste_is_safe("ls\r", false));
        assert!(!paste_is_safe("a\x1b[201~rm\n", true));

        let (view, mut rx, cx) = terminal(cx);
        let frame = |seq, modes| {
            TermEvent::Frame(Frame {
                seq,
                full: true,
                epoch: 0,
                cols: 10,
                rows: 3,
                cursor: Cursor::default(),
                modes,
                oldest_line: LineIndex(0),
                first_visible_line: LineIndex(0),
                total_lines: 3,
                input_ack: 0,
                images: Vec::new(),
                updates: vec![RowUpdate {
                    row: 0,
                    line: Line::from_text("$ ", 10, Style::DEFAULT),
                }],
            })
        };
        let pastes = |rx: &mut mpsc::Receiver<ClientMsg>| {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|msg| match msg {
                    ClientMsg::Term { req: TermRequest::Paste(text), .. } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let put = |cx: &mut VisualTestContext, text: &str| {
            cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.into())));
        };
        view.update_in(cx, |view, _window, cx| view.apply(frame(1, TermModes::empty()), cx));
        cx.run_until_parked();

        put(cx, "make\nrm -rf build\n");
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        assert!(pastes(&mut rx).is_empty(), "held back");
        assert_eq!(
            view.read_with(cx, |v, _| v.pending_paste().map(str::to_owned)).as_deref(),
            Some("make\nrm -rf build\n")
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(pastes(&mut rx), ["make\nrm -rf build\n"], "\u{21a9} sends it whole");
        assert!(view.read_with(cx, |v, _| v.pending_paste().is_none()));

        cx.simulate_keystrokes("cmd-v");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(
            pastes(&mut rx).is_empty() && view.read_with(cx, |v, _| v.pending_paste().is_none()),
            "Esc drops it"
        );

        cx.simulate_keystrokes("cmd-v");
        cx.simulate_keystrokes("x");
        cx.run_until_parked();
        assert!(pastes(&mut rx).is_empty(), "another key drops it");
        assert!(view.read_with(cx, |v, _| v.pending_paste().is_none()));

        put(cx, "one line");
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        assert_eq!(pastes(&mut rx), ["one line"], "nothing to run: straight through");

        view.update_in(cx, |view, _window, cx| {
            view.apply(frame(2, TermModes::BRACKETED_PASTE), cx);
        });
        put(cx, "make\nrm -rf build\n");
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        assert_eq!(pastes(&mut rx).len(), 1, "bracketed: the program sees a paste, not keys");

        view.update_in(cx, |view, _window, cx| view.apply(frame(3, TermModes::empty()), cx));
        view.update_in(cx, |view, _window, cx| {
            let mut theme = Theme::new(slopty_theme::Variant::Dark);
            theme.behaviour.paste_protection = false;
            view.set_theme(theme, cx);
        });
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        assert_eq!(pastes(&mut rx).len(), 1, "protection off: straight through");
    }

    /// ⌘W on a shell whose command is running asks first: the canvas's `ask_close` puts the
    /// bar up and says so; ↩ confirms (the canvas hears `CloseConfirmed`), Esc keeps the
    /// shell, any other key keeps it and goes to the program; an idle shell, a shell with
    /// the setting off, never ask.
    #[gpui::test]
    fn closing_a_busy_shell_asks_first(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let confirmed = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        cx.update(|_window, cx| {
            let confirmed = std::rc::Rc::clone(&confirmed);
            cx.subscribe(&view, move |_view, event, _cx| {
                if matches!(event, TerminalViewEvent::CloseConfirmed) {
                    confirmed.set(confirmed.get().saturating_add(1));
                }
            })
            .detach();
        });
        let frame = |seq, rows: &[(&str, SemanticMark)], cursor_row| {
            TermEvent::Frame(Frame {
                seq,
                full: true,
                epoch: 0,
                cols: 10,
                rows: 3,
                cursor: Cursor { row: cursor_row, ..Cursor::default() },
                modes: TermModes::empty(),
                oldest_line: LineIndex(0),
                first_visible_line: LineIndex(0),
                total_lines: 3,
                input_ack: 0,
                images: Vec::new(),
                updates: rows
                    .iter()
                    .enumerate()
                    .map(|(row, (text, mark))| {
                        let mut line = Line::from_text(text, 10, Style::DEFAULT);
                        line.mark = *mark;
                        RowUpdate { row: u16::try_from(row).unwrap(), line }
                    })
                    .collect(),
            })
        };
        let prompt = SemanticMark::Prompt { exit: None, input: Some(2) };
        // Idle at the prompt: nothing to ask.
        view.update_in(cx, |view, _window, cx| {
            view.apply(frame(1, &[("$ ", prompt), ("", SemanticMark::Output)], 0), cx);
        });
        assert!(!view.update(cx, TerminalView::ask_close), "idle: closes at once");
        // Running: the bar asks, ↩ confirms.
        view.update_in(cx, |view, _window, cx| {
            view.apply(frame(2, &[("$ make", prompt), ("", SemanticMark::Output)], 1), cx);
        });
        assert!(view.read_with(cx, |v, _| v.state.command_running()));
        assert!(view.update(cx, TerminalView::ask_close), "running: asks");
        cx.run_until_parked();
        assert!(cx.debug_bounds("close-confirm").is_some(), "the bar is up");
        assert!(view.read_with(cx, |v, _| v.close_asked()));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(confirmed.get(), 1, "\u{21a9} confirms");
        assert!(!view.read_with(cx, |v, _| v.close_asked()));
        assert!(drain_input(&mut rx).is_empty(), "the \u{21a9} was not typed");
        // Esc keeps it.
        assert!(view.update(cx, TerminalView::ask_close));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!view.read_with(cx, |v, _| v.close_asked()), "Esc keeps the shell");
        assert_eq!(confirmed.get(), 1);
        assert!(drain_input(&mut rx).is_empty());
        // Any other key keeps it and goes on to the program.
        assert!(view.update(cx, TerminalView::ask_close));
        cx.simulate_keystrokes("x");
        cx.run_until_parked();
        assert!(!view.read_with(cx, |v, _| v.close_asked()));
        assert_eq!(drain_input(&mut rx), ["x"], "the key reached the program");
        assert_eq!(confirmed.get(), 1);
        // The setting off: closes at once.
        view.update_in(cx, |view, _window, cx| {
            let mut theme = Theme::new(slopty_theme::Variant::Dark);
            theme.behaviour.confirm_close = false;
            view.set_theme(theme, cx);
        });
        assert!(!view.update(cx, TerminalView::ask_close), "off: closes at once");
    }

    /// ⌘⇧C copies the output of the last finished command; with no marks it copies nothing.
    #[gpui::test]
    fn cmd_shift_c_copies_the_last_commands_output(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("before".into())));
        cx.simulate_keystrokes("cmd-shift-c");
        let text = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()))
        };
        assert_eq!(text(cx).as_deref(), Some("before"), "no prompts: the clipboard is untouched");

        with_command_blocks(&view, cx);
        cx.simulate_keystrokes("cmd-shift-c");
        assert_eq!(text(cx).as_deref(), Some("1\n2"), "blank tail trimmed, prompt rows excluded");

        // ⌘K is the host's to do: one request, nothing typed.
        drain_words(&mut rx);
        cx.simulate_keystrokes("cmd-k");
        assert_eq!(drain_words(&mut rx), ["clear"]);
    }

    /// A right click on a block's row opens its menu: the typed command and the output to
    /// the clipboard, the command run again (a paste, then ↩), the block selected; Esc and
    /// any click close it; the terminal's own items come after the block's.
    #[gpui::test]
    fn a_right_click_on_a_block_offers_its_command_and_output(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        with_command_blocks(&view, cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        drain_words(&mut rx);
        // Row 0 of the viewport is line 6 ("2"), output of `seq 2` (prompt line 4).
        let at = view.read_with(cx, |v, _| {
            let m = v.metrics.expect("laid out");
            m.origin + point(m.cell_width * 1.5, m.line_height * 0.5)
        });
        let right_click = |cx: &mut VisualTestContext| {
            cx.simulate_mouse_down(at, MouseButton::Right, gpui::Modifiers::default());
            cx.run_until_parked();
        };
        let pick = |cx: &mut VisualTestContext, key: &'static str| {
            let selector: &'static str = Box::leak(format!("block-menu-{key}").into_boxed_str());
            let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{key} in the menu"));
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
            cx.run_until_parked();
            assert!(cx.debug_bounds("block-menu").is_none(), "the menu closes after {key}");
        };
        let clipboard = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()))
        };

        right_click(cx);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Menu", Some("Command block"))), "{tree:#?}");
        for label in [
            "Copy command",
            "Copy output",
            "Rerun",
            "Save as note",
            "Select block",
            "Paste",
            "Find…",
            "Clear screen",
        ] {
            assert!(tree.iter().any(|n| n.is("MenuItem", Some(label))), "{label}: {tree:#?}");
        }
        assert!(!tree.iter().any(|n| n.is("MenuItem", Some("Copy"))), "nothing selected yet");
        let block = view.read_with(cx, |v, _| v.block_menu.as_ref().and_then(|m| m.block.clone()));
        let block = block.expect("the menu holds its block");
        assert_eq!(block.command.as_deref(), Some("seq 2"));
        assert_eq!(block.output, "1\n2");
        assert!(drain_input(&mut rx).is_empty(), "a right click on a block is not reported");
        pick(cx, "copy-output");
        assert_eq!(clipboard(cx).as_deref(), Some("1\n2"));

        right_click(cx);
        pick(cx, "copy-command");
        assert_eq!(clipboard(cx).as_deref(), Some("seq 2"));

        right_click(cx);
        pick(cx, "rerun");
        assert_eq!(drain_input(&mut rx), ["paste:seq 2", "enter"]);

        let notes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&notes);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::NoteBlock(text) = event {
                    seen.borrow_mut().push(text.clone());
                }
            })
            .detach();
        });
        right_click(cx);
        pick(cx, "note");
        assert_eq!(notes.borrow().as_slice(), ["# seq 2\n\n```sh\nseq 2\n```\n\n```\n1\n2\n```\n"]);
        assert!(drain_input(&mut rx).is_empty(), "a note is not typed into the shell");
        // The palette's line needs no click on a row: the last finished block.
        cx.update(|window, cx| window.dispatch_action(Box::new(NoteLastBlock), cx));
        cx.run_until_parked();
        assert_eq!(notes.borrow().len(), 2);
        assert_eq!(notes.borrow()[1], "# seq 2\n\n```sh\nseq 2\n```\n\n```\n1\n2\n```\n");

        right_click(cx);
        pick(cx, "select-block");
        let selected = view.read_with(cx, |v, _| v.selected_text());
        assert_eq!(selected.as_deref(), Some("$ seq 2\n1\n2\n"), "prompt row to the blank row");

        right_click(cx);
        cx.simulate_keystrokes("escape");
        assert!(cx.debug_bounds("block-menu").is_none(), "Esc closes it");
        assert!(drain_input(&mut rx).is_empty(), "the Esc went to the menu, not the shell");
        right_click(cx);
        // Left of the menu (it hangs right and down from the click), on another row.
        let elsewhere = view.read_with(cx, |v, _| {
            let m = v.metrics.expect("laid out");
            m.origin + point(m.cell_width * 0.5, m.line_height * 2.5)
        });
        cx.simulate_click(elsewhere, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("block-menu").is_none(), "a click elsewhere closes it");

        // The phone's armed ⌘ then a tap on a block row with no link or path under it opens
        // the same menu, one tap, nothing to the program.
        drain_input(&mut rx);
        view.update(cx, |v, cx| v.set_sticky_command(true, cx));
        cx.simulate_click(at, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("block-menu").is_some(), "the armed tap opens the menu");
        assert!(!view.read_with(cx, |v, _| v.sticky_command()), "one tap");
        let tapped = view.read_with(cx, |v, _| v.block_menu.as_ref().and_then(|m| m.block.clone()));
        assert_eq!(tapped.and_then(|b| b.command).as_deref(), Some("seq 2"));
        assert!(drain_input(&mut rx).is_empty(), "the tap is not reported");
        pick(cx, "copy-command");
        assert_eq!(clipboard(cx).as_deref(), Some("seq 2"));
        // Disarmed, a tap on the same row is a plain click: no menu.
        cx.simulate_click(at, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("block-menu").is_none());
    }

    /// A right click off any block (no shell integration, a program's output) still offers
    /// the terminal's own items: Paste, Find, Clear screen, and Copy once something is
    /// selected; each does what its shortcut does and closes the menu.
    #[gpui::test]
    fn a_right_click_off_a_block_offers_the_terminals_own_items(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let rows = ["hello wor", "second", "third row"];
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
                    images: Vec::new(),
                    updates: rows
                        .iter()
                        .enumerate()
                        .map(|(row, text)| RowUpdate {
                            row: u16::try_from(row).unwrap(),
                            line: marked(text, SemanticMark::Output),
                        })
                        .collect(),
                }),
                cx,
            );
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        drain_input(&mut rx);
        let at = view.read_with(cx, |v, _| {
            let m = v.metrics.expect("laid out");
            m.origin + point(m.cell_width * 1.5, m.line_height * 1.5)
        });
        let right_click = |cx: &mut VisualTestContext| {
            cx.simulate_mouse_down(at, MouseButton::Right, gpui::Modifiers::default());
            cx.run_until_parked();
        };
        let pick = |cx: &mut VisualTestContext, key: &'static str| {
            let selector: &'static str = Box::leak(format!("block-menu-{key}").into_boxed_str());
            let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{key} in the menu"));
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
            cx.run_until_parked();
            assert!(cx.debug_bounds("block-menu").is_none(), "the menu closes after {key}");
        };

        right_click(cx);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Menu", Some("Terminal"))), "{tree:#?}");
        let items: Vec<_> =
            tree.iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label.clone()).collect();
        assert_eq!(items, ["Paste", "Find…", "Clear screen"]);
        assert!(drain_input(&mut rx).is_empty(), "a right click is not reported");
        pick(cx, "clear");
        assert_eq!(drain_words(&mut rx), ["clear"], "the host clears, nothing typed");

        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("hi".into())));
        right_click(cx);
        pick(cx, "paste");
        assert_eq!(drain_input(&mut rx), ["paste:hi"]);

        view.update(cx, |v, cx| {
            v.selection = Some(Selection::run((LineIndex(1), 0), (LineIndex(1), 5)));
            cx.notify();
        });
        right_click(cx);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let items: Vec<_> =
            tree.iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label.clone()).collect();
        assert_eq!(items, ["Copy", "Save as note", "Paste", "Find…", "Clear screen"]);
        pick(cx, "copy");
        let text = cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()));
        assert_eq!(text.as_deref(), Some("second"));

        // The selection, fenced, is what the note keeps.
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&events);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::NoteBlock(text) = event {
                    seen.borrow_mut().push(format!("note:{text}"));
                }
            })
            .detach();
        });
        right_click(cx);
        pick(cx, "note");
        assert_eq!(events.borrow().as_slice(), ["note:```\nsecond\n```\n"]);

        // ⇧-arrows move the selection's head; the shell sees none of them. Without a
        // selection the same keys are the shell's.
        cx.simulate_keystrokes("shift-right shift-down");
        let head =
            |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.selection.map(|s| s.head));
        assert_eq!(head(cx), Some((LineIndex(2), 6)));
        cx.simulate_keystrokes("shift-left shift-up");
        assert_eq!(head(cx), Some((LineIndex(1), 5)));
        cx.simulate_keystrokes("shift-right shift-right shift-right shift-right shift-right");
        assert_eq!(head(cx), Some((LineIndex(2), 0)), "past the row's end, the next row");
        cx.simulate_keystrokes("shift-left");
        assert_eq!(head(cx), Some((LineIndex(1), 9)));
        cx.simulate_keystrokes("shift-down shift-down shift-down");
        assert_eq!(head(cx), Some((LineIndex(2), 9)), "no line below the newest");
        assert!(drain_input(&mut rx).is_empty(), "the shell saw no arrow");
        cx.simulate_keystrokes("escape");
        assert_eq!(head(cx), None, "a plain key drops the selection");
        drain_input(&mut rx);
        cx.simulate_keystrokes("shift-right");
        assert_eq!(drain_input(&mut rx), ["arrowright"], "no selection: the shell's key");

        right_click(cx);
        pick(cx, "find");
        assert!(view.read_with(cx, |v, _| v.search.is_some()), "the find field opened");
    }

    #[test]
    fn a_took_label_reads_as_a_clock_would() {
        assert_eq!(took_label(Duration::from_millis(1_040)), "1.0 s");
        assert_eq!(took_label(Duration::from_millis(3_260)), "3.3 s", "a tenth, rounded");
        assert_eq!(took_label(Duration::from_secs(59)), "59.0 s");
        assert_eq!(took_label(Duration::from_secs(60)), "1 m 00 s");
        assert_eq!(took_label(Duration::from_secs(123)), "2 m 03 s");
        assert_eq!(took_label(Duration::from_secs(3_599)), "59 m 59 s");
        assert_eq!(took_label(Duration::from_secs(3_600)), "1 h 00 m");
        assert_eq!(took_label(Duration::from_mins(362)), "6 h 02 m");
    }

    /// A finished command's row says how long it took, at its right end, once it took a
    /// second or more; the caption follows the row through history and a new epoch (a
    /// reflow renumbers the rows) forgets them all.
    #[gpui::test]
    fn a_slow_commands_row_says_how_long_it_took(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        with_command_blocks(&view, cx);
        let captions = |cx: &mut VisualTestContext| {
            view.update(cx, |_v, cx| cx.notify());
            cx.run_until_parked();
            cx.update(|_window, cx| crate::terminal::captions_drawn(cx))
        };
        assert!(captions(cx).is_empty());
        view.update(cx, |v, _cx| {
            v.set_took(LineIndex(0), Duration::from_millis(3_260));
            v.set_took(LineIndex(3), Duration::from_millis(400));
            v.set_took(LineIndex(4), Duration::from_secs(2));
        });
        assert_eq!(view.read_with(cx, |v, _| v.took(LineIndex(3))), None, "under a second");
        assert!(captions(cx).is_empty(), "the captioned rows are above the viewport");
        // Ten columns: `$ seq 2` reaches the caption's cells, so the text wins.
        cx.simulate_keystrokes("cmd-up");
        assert_eq!(top_line(&view, cx), LineIndex(4));
        assert!(captions(cx).is_empty(), "`$ seq 2` at the top: its text reaches the caption");
        cx.simulate_keystrokes("cmd-up");
        cx.simulate_keystrokes("cmd-up");
        assert_eq!(top_line(&view, cx), LineIndex(0));
        assert_eq!(captions(cx), ["3.3 s"], "`$ ls` leaves room: its row says how long it took");
        // A new epoch renumbers the rows: nothing said before applies.
        view.update_in(cx, |view, _window, cx| {
            let prompt = SemanticMark::Prompt { exit: Some(0), input: Some(2) };
            let mut frame = Frame {
                seq: 9,
                full: true,
                epoch: 1,
                cols: 20,
                rows: 3,
                cursor: Cursor::default(),
                modes: TermModes::empty(),
                oldest_line: LineIndex(0),
                first_visible_line: LineIndex(0),
                total_lines: 3,
                input_ack: 0,
                images: Vec::new(),
                updates: vec![RowUpdate {
                    row: 0,
                    line: Line::from_text("$ ", 20, Style::DEFAULT),
                }],
            };
            frame.updates[0].line.mark = prompt;
            view.apply(TermEvent::Frame(frame), cx);
        });
        assert_eq!(view.read_with(cx, |v, _| v.took(LineIndex(4))), None, "a new epoch forgets");
        assert!(captions(cx).is_empty());
    }

    #[test]
    fn a_block_note_keeps_the_half_it_has() {
        let block = |command: Option<&str>, output: &str| CommandBlock {
            prompt: LineIndex(0),
            end: LineIndex(1),
            exit: None,
            command: command.map(str::to_owned),
            output: output.to_owned(),
        };
        assert_eq!(block_note(&block(Some("ls"), "")), "# ls\n\n```sh\nls\n```\n");
        assert_eq!(block_note(&block(None, "a\nb")), "```\na\nb\n```\n");
        assert_eq!(
            block_note(&block(Some("for x in 1 2\ndo echo $x\ndone"), "1\n2")),
            "# for x in 1 2\n\n```sh\nfor x in 1 2\ndo echo $x\ndone\n```\n\n```\n1\n2\n```\n",
            "a multi-line command is headed by its first line"
        );
        assert_eq!(block_note(&block(None, "")), "");
    }

    #[test]
    fn selection_columns_cover_edges_and_middle_lines() {
        let s = Selection::run((LineIndex(7), 5), (LineIndex(5), 2));
        assert_eq!(s.columns(LineIndex(4), 10), None);
        assert_eq!(s.columns(LineIndex(5), 10), Some(2..10));
        assert_eq!(s.columns(LineIndex(6), 10), Some(0..10));
        assert_eq!(s.columns(LineIndex(7), 10), Some(0..6));
        assert_eq!(s.columns(LineIndex(8), 10), None);
        let one = Selection::run((LineIndex(1), 3), (LineIndex(1), 3));
        assert_eq!(one.columns(LineIndex(1), 10), Some(3..4));
    }

    /// A block selection is the rectangle between its corners: the same columns on every
    /// line, whichever corner the drag started from, and never past the grid's edge.
    #[test]
    fn a_block_selection_is_the_same_columns_on_every_line() {
        let s = Selection { anchor: (LineIndex(7), 5), head: (LineIndex(5), 2), block: true };
        assert_eq!(s.columns(LineIndex(4), 10), None);
        assert_eq!(s.columns(LineIndex(5), 10), Some(2..6));
        assert_eq!(s.columns(LineIndex(6), 10), Some(2..6));
        assert_eq!(s.columns(LineIndex(7), 10), Some(2..6));
        assert_eq!(s.columns(LineIndex(8), 10), None);
        let edge = Selection { anchor: (LineIndex(1), 9), head: (LineIndex(2), 12), block: true };
        assert_eq!(edge.columns(LineIndex(2), 10), Some(9..10));
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
                    images: Vec::new(),
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
            view.selection = Some(Selection::run((LineIndex(100), 6), (LineIndex(102), 4)));
            assert_eq!(view.selected_text().as_deref(), Some("wor\nsecond\nthird"));
            // Backwards drags read the same.
            view.selection = Some(Selection::run((LineIndex(102), 4), (LineIndex(100), 6)));
            assert_eq!(view.selected_text().as_deref(), Some("wor\nsecond\nthird"));
            // A block takes the same columns of every row.
            view.selection = Some(Selection {
                anchor: (LineIndex(100), 6),
                head: (LineIndex(102), 4),
                block: true,
            });
            assert_eq!(view.selected_text().as_deref(), Some("o w\nnd\nd r"));
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

    /// A frame of `rows` on a screen whose first line is `first`, with `first` lines of
    /// history before it (line 0 is the oldest kept).
    fn history_frame(first: u64, rows: &[&str]) -> TermEvent {
        TermEvent::Frame(Frame {
            seq: 1,
            full: true,
            epoch: 0,
            cols: 10,
            rows: u16::try_from(rows.len()).unwrap_or(3),
            cursor: Cursor::default(),
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(first),
            total_lines: first.saturating_add(u64::try_from(rows.len()).unwrap_or(3)),
            input_ack: 0,
            images: Vec::new(),
            updates: rows
                .iter()
                .enumerate()
                .map(|(row, text)| RowUpdate {
                    row: u16::try_from(row).unwrap_or(0),
                    line: Line::from_text(text, 10, Style::DEFAULT),
                })
                .collect(),
        })
    }

    /// The window point at the middle of cell (`col`, `row`).
    fn cell_center(
        view: &Entity<TerminalView>,
        cx: &VisualTestContext,
        col: f32,
        row: f32,
    ) -> gpui::Point<Pixels> {
        view.read_with(cx, |v, _| {
            let m = v.metrics.expect("laid out");
            m.origin + point(m.cell_width * (col + 0.5), m.line_height * (row + 0.5))
        })
    }

    /// A drag selects from press to release; ⇧-click afterwards moves the head, keeping the
    /// anchor; a plain click drops it all and starts over.
    #[gpui::test]
    fn a_shift_click_extends_the_selection(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
            view.apply(history_frame(0, &["hello wor", "second", "third row"]), cx);
        });
        cx.run_until_parked();
        let mods = gpui::Modifiers::default();
        let shift = gpui::Modifiers { shift: true, ..gpui::Modifiers::default() };
        cx.simulate_mouse_down(cell_center(&view, cx, 0.0, 0.0), MouseButton::Left, mods);
        cx.simulate_mouse_move(cell_center(&view, cx, 4.0, 0.0), MouseButton::Left, mods);
        cx.simulate_mouse_up(cell_center(&view, cx, 4.0, 0.0), MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.selected_text()).as_deref(), Some("hello"));

        cx.simulate_mouse_down(cell_center(&view, cx, 2.0, 2.0), MouseButton::Left, shift);
        cx.simulate_mouse_up(cell_center(&view, cx, 2.0, 2.0), MouseButton::Left, shift);
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.selected_text()).as_deref(),
            Some("hello wor\nsecond\nthi"),
            "the anchor stays, the head moves to the ⇧-click"
        );
        cx.simulate_mouse_down(cell_center(&view, cx, 1.0, 1.0), MouseButton::Left, shift);
        cx.simulate_mouse_up(cell_center(&view, cx, 1.0, 1.0), MouseButton::Left, shift);
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.selected_text()).as_deref(),
            Some("hello wor\nse"),
            "⇧-click again moves the head back"
        );
        cx.simulate_click(cell_center(&view, cx, 1.0, 1.0), mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.selected_text()), None, "a click clears it");
    }

    /// With `copy_on_select` on, a drag, a double-click and a ⇧-click each put the selection
    /// on the clipboard as they end; off (the default), only ⌘C does.
    #[gpui::test]
    fn a_selection_is_copied_as_it_is_made_when_asked(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
            view.apply(history_frame(0, &["hello wor", "second", "third row"]), cx);
        });
        cx.run_until_parked();
        let text = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()))
        };
        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("before".into())));
        let mods = gpui::Modifiers::default();
        let shift = gpui::Modifiers { shift: true, ..gpui::Modifiers::default() };
        cx.simulate_mouse_down(cell_center(&view, cx, 0.0, 0.0), MouseButton::Left, mods);
        cx.simulate_mouse_move(cell_center(&view, cx, 4.0, 0.0), MouseButton::Left, mods);
        cx.simulate_mouse_up(cell_center(&view, cx, 4.0, 0.0), MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.selected_text()).as_deref(), Some("hello"));
        assert_eq!(text(cx).as_deref(), Some("before"), "off: the clipboard is untouched");

        let mut theme = Theme::default();
        theme.behaviour.copy_on_select = true;
        view.update(cx, |view, cx| view.set_theme(theme, cx));
        cx.simulate_mouse_down(cell_center(&view, cx, 2.0, 2.0), MouseButton::Left, shift);
        cx.simulate_mouse_up(cell_center(&view, cx, 2.0, 2.0), MouseButton::Left, shift);
        cx.run_until_parked();
        assert_eq!(text(cx).as_deref(), Some("hello wor\nsecond\nthi"), "\u{21e7}-click copies");
        cx.simulate_mouse_down(cell_center(&view, cx, 1.0, 1.0), MouseButton::Left, mods);
        cx.simulate_mouse_move(cell_center(&view, cx, 3.0, 1.0), MouseButton::Left, mods);
        cx.simulate_mouse_up(cell_center(&view, cx, 3.0, 1.0), MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(text(cx).as_deref(), Some("eco"), "a drag copies on release");
        cx.simulate_click(cell_center(&view, cx, 5.0, 2.0), mods);
        cx.run_until_parked();
        assert_eq!(text(cx).as_deref(), Some("eco"), "a click selects nothing, copies nothing");
        let at = cell_center(&view, cx, 7.0, 0.0);
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: mods,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_mouse_up(at, MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(text(cx).as_deref(), Some("wor"), "a double-click copies the word");
    }

    /// A plain click on the shell's input line sends the arrow keys that put the cursor
    /// under it; a click on output, a drag, or a modified click sends nothing.
    #[gpui::test]
    fn a_click_on_the_input_line_moves_the_cursor_there(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let mut f = history_frame(0, &["out", "$ abcdef", ""]);
        if let TermEvent::Frame(frame) = &mut f {
            frame.updates[1].line.mark = SemanticMark::Prompt { exit: Some(0), input: Some(2) };
            frame.updates[2].line.mark = SemanticMark::Input;
            frame.updates[2].line.flags |= slopty_grid::LineFlags::WRAPPED;
            frame.cursor = Cursor { row: 1, col: 8, visible: true, ..Cursor::default() };
        }
        view.update_in(cx, |view, _window, cx| view.apply(f, cx));
        cx.run_until_parked();
        let mods = gpui::Modifiers::default();
        cx.simulate_click(cell_center(&view, cx, 3.0, 1.0), mods);
        cx.run_until_parked();
        assert_eq!(drain_input(&mut rx), ["arrowleft"; 5], "five cells back");
        cx.simulate_click(cell_center(&view, cx, 0.0, 1.0), mods);
        cx.run_until_parked();
        assert_eq!(drain_input(&mut rx), ["arrowleft"; 6], "the prompt's text: held to the input");
        cx.simulate_click(cell_center(&view, cx, 1.0, 0.0), mods);
        cx.run_until_parked();
        assert!(drain_input(&mut rx).is_empty(), "output is not the line editor");
        cx.simulate_mouse_down(cell_center(&view, cx, 2.0, 1.0), MouseButton::Left, mods);
        cx.simulate_mouse_move(cell_center(&view, cx, 5.0, 1.0), MouseButton::Left, mods);
        cx.simulate_mouse_up(cell_center(&view, cx, 5.0, 1.0), MouseButton::Left, mods);
        cx.run_until_parked();
        assert!(drain_input(&mut rx).is_empty(), "a drag selects");
        let alt = gpui::Modifiers { alt: true, ..gpui::Modifiers::default() };
        cx.simulate_click(cell_center(&view, cx, 3.0, 1.0), alt);
        cx.run_until_parked();
        assert!(drain_input(&mut rx).is_empty(), "a modified click is not a plain one");
        let mut alt_screen = history_frame(0, &["out", "$ abcdef", ""]);
        if let TermEvent::Frame(frame) = &mut alt_screen {
            frame.seq = 2;
            frame.modes = TermModes::ALT_SCREEN;
            frame.updates[1].line.mark = SemanticMark::Prompt { exit: Some(0), input: Some(2) };
            frame.cursor = Cursor { row: 1, col: 8, visible: true, ..Cursor::default() };
        }
        view.update_in(cx, |view, _window, cx| view.apply(alt_screen, cx));
        cx.run_until_parked();
        cx.simulate_click(cell_center(&view, cx, 3.0, 1.0), mods);
        cx.run_until_parked();
        assert!(drain_input(&mut rx).is_empty(), "a full-screen program gets no arrows");
    }

    /// Dragging a selection above the grid keeps scrolling into history a tick at a time,
    /// the head riding the top row; back inside it stops; the release ends it.
    #[gpui::test]
    fn a_drag_past_the_top_scrolls_into_history(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
            view.apply(history_frame(100, &["hello wor", "second", "third row"]), cx);
        });
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        let mods = gpui::Modifiers::default();
        cx.simulate_mouse_down(cell_center(&view, cx, 3.0, 1.0), MouseButton::Left, mods);
        // Two rows above the grid: two lines a tick.
        let above = cell_center(&view, cx, 3.0, -2.0);
        cx.simulate_mouse_move(above, MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.autoscroll), Some((2, 3)));
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 0, "nothing until a tick");
        cx.executor().advance_clock(Duration::from_millis(55));
        cx.run_until_parked();
        let (offset, selection) =
            view.read_with(cx, |v, _| (v.state.view_offset(), v.selection.map(Selection::ordered)));
        assert_eq!(offset, 2, "one tick, two lines");
        assert_eq!(
            selection,
            Some(((LineIndex(98), 3), (LineIndex(101), 3))),
            "head on the top row"
        );
        let fetched = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|msg| {
                matches!(msg, ClientMsg::Term { req: TermRequest::FetchLines { .. }, .. })
            })
            .count();
        assert!(fetched > 0, "the lines scrolled into view were asked for");
        cx.executor().advance_clock(Duration::from_millis(55));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 4, "it keeps going");

        // Back over the grid: the pace stops, the head follows the pointer.
        cx.simulate_mouse_move(cell_center(&view, cx, 5.0, 2.0), MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.autoscroll), None);
        cx.executor().advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 4, "no more scrolling");
        assert_eq!(
            view.read_with(cx, |v, _| v.selection.map(|s| s.head)),
            Some((LineIndex(98), 5)),
            "row 2 of a viewport scrolled by 4 is line 98"
        );
        // Below the grid scrolls back down, and the release ends everything.
        let below = view.read_with(cx, |v, _| {
            let m = v.metrics.expect("laid out");
            m.origin + point(m.cell_width * 5.5, m.line_height * (f32::from(m.rows) + 0.5))
        });
        cx.simulate_mouse_move(below, MouseButton::Left, mods);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(55));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 3, "one line down");
        cx.simulate_mouse_up(below, MouseButton::Left, mods);
        cx.executor().advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| (v.autoscroll, v.selecting)), (None, false));
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 3);
    }

    /// The scroll multiplier: three grid lines per wheel line when the theme says so.
    #[gpui::test]
    fn the_wheel_scrolls_by_the_multiplier(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
            view.apply(history_frame(100, &["hello wor", "second", "third row"]), cx);
            let mut theme = Theme::new(slopty_theme::Variant::Dark);
            theme.behaviour.scroll_multiplier = 300;
            view.set_theme(theme, cx);
        });
        cx.run_until_parked();
        let at = cell_center(&view, cx, 2.0, 1.0);
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Lines(point(0.0, 1.0)),
            modifiers: gpui::Modifiers::default(),
            touch_phase: TouchPhase::Started,
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 3);
    }

    /// ⇧⇞ / ⇧⇟ page through history, ⇧⇱ / ⌘⇱ go to the oldest line and ⇧⇲ / ⌘⇲ back to the
    /// output; each fetches what the cache lacks. ⌘A selects it all and fetches the rest so
    /// ⌘C has every line.
    #[gpui::test]
    fn the_keys_page_through_history_and_select_it_all(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
            view.apply(history_frame(100, &["hello wor", "second", "third row"]), cx);
        });
        cx.run_until_parked();
        drain_words(&mut rx);
        let offset = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.state.view_offset());
        let fetches = |rx: &mut mpsc::Receiver<ClientMsg>| {
            let mut out = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                if let ClientMsg::Term { req: TermRequest::FetchLines { start, count }, .. } = msg {
                    out.push((start.0, count));
                }
            }
            out
        };

        cx.simulate_keystrokes("shift-pageup");
        cx.run_until_parked();
        assert_eq!(offset(cx), 3, "a page is the viewport's rows");
        assert_eq!(fetches(&mut rx), [(97, 3)]);
        cx.simulate_keystrokes("shift-pagedown");
        cx.run_until_parked();
        assert_eq!(offset(cx), 0);
        cx.simulate_keystrokes("cmd-home");
        cx.run_until_parked();
        assert_eq!(offset(cx), 100, "the top is the oldest line");
        assert_eq!(fetches(&mut rx), [(0, 3)]);
        cx.simulate_keystrokes("cmd-end");
        cx.run_until_parked();
        assert_eq!(offset(cx), 0);
        cx.simulate_keystrokes("shift-home");
        cx.run_until_parked();
        assert_eq!(offset(cx), 100);
        cx.simulate_keystrokes("shift-end");
        cx.run_until_parked();
        assert_eq!(offset(cx), 0);
        assert!(drain_words(&mut rx).iter().all(|w| w.starts_with("fetch")), "no keys typed");

        cx.simulate_keystrokes("cmd-a");
        cx.run_until_parked();
        let selection = view.read_with(cx, |v, _| v.selection.map(Selection::ordered));
        assert_eq!(selection, Some(((LineIndex(0), 0), (LineIndex(102), 9))));
        assert_eq!(fetches(&mut rx), [(0, 100)], "the history not cached yet");
        view.update_in(cx, |view, _window, cx| {
            view.apply(
                TermEvent::Lines {
                    start: LineIndex(98),
                    lines: vec![
                        Line::from_text("older", 10, Style::DEFAULT),
                        Line::from_text("old", 10, Style::DEFAULT),
                    ],
                },
                cx,
            );
        });
        cx.simulate_keystrokes("cmd-c");
        cx.run_until_parked();
        let text = cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()));
        assert_eq!(
            text.as_deref().map(|t| t.trim_start_matches('\n')),
            Some("older\nold\nhello wor\nsecond\nthird row"),
            "what arrived, in order; lines never fetched are blank"
        );
    }

    /// A trackpad's fractions of a line add up to whole lines scrolled, and a gesture stays
    /// with the surface that took its first event; a program tracking the mouse gets the wheel
    /// as rows (⇧ keeps it local); so does the alternate screen.
    #[gpui::test]
    fn the_wheel_adds_up_fractions_and_reaches_a_program_that_wants_it(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let frame = |modes| match history_frame(100, &["hello wor", "second", "third row"]) {
            TermEvent::Frame(mut f) => {
                f.modes = modes;
                TermEvent::Frame(f)
            }
            other => other,
        };
        view.update_in(cx, |view, _window, cx| view.apply(frame(TermModes::empty()), cx));
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        let at = cell_center(&view, cx, 2.0, 1.0);
        let wheel = |cx: &mut VisualTestContext, lines: f32, modifiers, phase| {
            cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Lines(point(0.0, lines)),
                modifiers,
                touch_phase: phase,
            });
            cx.run_until_parked();
        };
        let mods = gpui::Modifiers::default();
        let offset = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.state.view_offset());
        wheel(cx, 0.4, mods, TouchPhase::Started);
        wheel(cx, 0.4, mods, TouchPhase::Moved);
        assert_eq!(offset(cx), 0, "0.8 of a line is not a line yet");
        wheel(cx, 0.4, mods, TouchPhase::Moved);
        assert_eq!(offset(cx), 1, "1.2: one line, 0.2 carried");
        wheel(cx, 0.9, mods, TouchPhase::Moved);
        assert_eq!(offset(cx), 2, "1.1: one more");
        wheel(cx, 0.9, mods, TouchPhase::Started);
        assert_eq!(offset(cx), 2, "a new gesture drops the 0.1 carried");
        wheel(cx, -3.0, mods, TouchPhase::Started);
        assert_eq!(offset(cx), 0, "and down again");
        wheel(cx, -0.7, mods, TouchPhase::Started);
        wheel(cx, -0.7, mods, TouchPhase::Moved);
        assert_eq!(offset(cx), 0, "no history below: the wheel passes to the canvas");
        assert!(
            view.read_with(cx, |v, _| v.wheel_remainder).abs() < f32::EPSILON,
            "and carries nothing"
        );

        // A trackpad sends pixels, and those events are a gesture: whichever surface could use
        // the first of them keeps the rest, momentum included.
        let pan = |cx: &mut VisualTestContext, lines: f32, phase| {
            let h = view.read_with(cx, |v, _| v.metrics.map_or(20.0, |m| f32::from(m.line_height)));
            cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(0.0), px(lines * h))),
                modifiers: mods,
                touch_phase: phase,
            });
            cx.run_until_parked();
        };
        let owner = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.wheel_gesture);
        pan(cx, 0.0, TouchPhase::Started);
        assert_eq!(owner(cx), None, "a finger landing has moved nothing and decides nothing");
        pan(cx, 3.0, TouchPhase::Moved);
        assert!(offset(cx) > 0, "the first movement puts the gesture in the grid");
        pan(cx, -99.0, TouchPhase::Moved);
        assert_eq!(offset(cx), 0, "which runs it to the bottom of the history");
        pan(cx, 0.0, TouchPhase::Ended);
        pan(cx, -99.0, TouchPhase::Moved);
        assert_eq!(
            owner(cx),
            Some(true),
            "the momentum after the fingers lift is still the grid's, not a canvas pan"
        );
        // And the other way: a pan the canvas began is not taken back the moment the grid
        // could use it, so dragging the canvas past a terminal never snags halfway across.
        pan(cx, 0.0, TouchPhase::Started);
        pan(cx, -1.0, TouchPhase::Moved);
        assert_eq!(owner(cx), Some(false), "nothing below: the gesture is the canvas's");
        pan(cx, 99.0, TouchPhase::Moved);
        assert_eq!(offset(cx), 0, "the grid stays out of a gesture it did not take");
        pan(cx, 0.0, TouchPhase::Ended);
        pan(cx, 99.0, TouchPhase::Moved);
        assert_eq!(offset(cx), 0, "momentum the other way round, and still not the grid's");
        // A mouse wheel has no gesture: the latch says "canvas" and the notch scrolls anyway.
        wheel(cx, 3.0, mods, TouchPhase::Moved);
        assert_eq!(offset(cx), 3, "a wheel notch belongs to nothing and is judged on its own");
        wheel(cx, -3.0, mods, TouchPhase::Started);
        assert_eq!(offset(cx), 0, "back to the output");

        let cmd = gpui::Modifiers { platform: true, ..gpui::Modifiers::default() };
        wheel(cx, 3.0, cmd, TouchPhase::Started);
        assert_eq!(offset(cx), 0, "⌘-wheel is the canvas's zoom");
        let wheels = |rx: &mut mpsc::Receiver<ClientMsg>| {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|msg| match msg {
                    ClientMsg::Term {
                        req:
                            TermRequest::Mouse(MouseEvent {
                                action: MouseAction::Wheel { rows, .. },
                                ..
                            }),
                        ..
                    } => Some(rows),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(wheels(&mut rx).is_empty(), "nothing went to the program");

        view.update_in(cx, |view, _window, cx| view.apply(frame(TermModes::MOUSE_TRACKING), cx));
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        wheel(cx, 2.0, mods, TouchPhase::Started);
        assert_eq!(wheels(&mut rx), [2], "the program gets the rows");
        assert_eq!(offset(cx), 0, "and the viewport stays");
        // A finger landing inside a program that wants the mouse takes the gesture there and
        // then: the program has it whichever way it goes, so there is nothing to wait to see,
        // and the canvas never gets an event out of the middle of it.
        pan(cx, 0.0, TouchPhase::Started);
        assert_eq!(owner(cx), Some(true), "the program owns it from the landing");
        let shift = gpui::Modifiers { shift: true, ..gpui::Modifiers::default() };
        wheel(cx, 2.0, shift, TouchPhase::Started);
        assert!(wheels(&mut rx).is_empty(), "⇧ keeps the wheel");
        assert_eq!(offset(cx), 2);

        view.update_in(cx, |view, _window, cx| view.apply(frame(TermModes::ALT_SCREEN), cx));
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        wheel(cx, -1.0, mods, TouchPhase::Started);
        assert_eq!(wheels(&mut rx), [-1], "the alternate screen: the host makes it a key");
    }

    /// The scrollbar shows over the right edge once there is history and the pointer is over
    /// the grid or the viewport is scrolled; its thumb drags the viewport, a click on the
    /// track beside it pages, and the text under the bar is not selected by those clicks.
    #[gpui::test]
    fn the_scrollbar_drags_and_pages_the_viewport(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
            view.apply(history_frame(0, &["hello wor", "second", "third row"]), cx);
        });
        cx.run_until_parked();
        let mods = gpui::Modifiers::default();
        cx.simulate_mouse_move(cell_center(&view, cx, 1.0, 1.0), None, mods);
        cx.run_until_parked();
        assert!(!view.read_with(cx, |v, _| v.scrollbar_shown()), "no history, no bar");

        view.update_in(cx, |view, _window, cx| {
            view.apply(history_frame(30, &["hello wor", "second", "third row"]), cx);
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.scrollbar_shown()), "history and a pointer over it");
        let thumb = view.read_with(cx, |v, _| v.thumb()).expect("a thumb");
        let m = view.read_with(cx, |v, _| v.metrics.expect("laid out"));
        let rows = m.rows;
        let track_bottom = m.origin.y + m.line_height * f32::from(rows);
        assert!(
            thumb.origin.y + thumb.size.height <= track_bottom + px(0.01),
            "at the bottom of the history the thumb sits at the track's end: {thumb:?}"
        );
        assert!(
            thumb.origin.x + thumb.size.width
                <= m.origin.x + m.cell_width * f32::from(m.cols) + px(0.01)
        );

        // Clicking the track above the thumb pages up one screen.
        let track_above = point(thumb.center().x, m.origin.y + px(1.0));
        cx.simulate_mouse_down(track_above, MouseButton::Left, mods);
        cx.simulate_mouse_up(track_above, MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), u64::from(rows));
        assert_eq!(view.read_with(cx, |v, _| v.selection), None, "the track does not select");

        // Dragging the thumb to the top of the track shows the oldest lines.
        let thumb = view.read_with(cx, |v, _| v.thumb()).expect("a thumb");
        let grab = thumb.center();
        cx.simulate_mouse_down(grab, MouseButton::Left, mods);
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.thumb_held()));
        cx.simulate_mouse_move(point(grab.x, m.origin.y - px(50.0)), MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 30, "the whole history");
        cx.simulate_mouse_move(point(grab.x, track_bottom + px(50.0)), MouseButton::Left, mods);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 0, "and back to the bottom");
        cx.simulate_mouse_up(point(grab.x, track_bottom + px(50.0)), MouseButton::Left, mods);
        cx.run_until_parked();
        assert!(!view.read_with(cx, |v, _| v.thumb_held()));
        assert_eq!(view.read_with(cx, |v, _| v.selection), None, "the thumb does not select");
    }

    /// ⌘← is `^A` on the wire and ⌥⌫ `ESC DEL`, as raw bytes; with the setting off the
    /// chords are not the terminal's (⌘← goes up to the app, ⌥⌫ to the encoder as a key).
    #[gpui::test]
    fn the_macs_editing_keys_edit_the_line(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let raw = |rx: &mut mpsc::Receiver<ClientMsg>| {
            let mut out = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    ClientMsg::Term { req: TermRequest::Raw(bytes), .. } => out.push(bytes),
                    ClientMsg::Term { req: TermRequest::Key(key), .. } => {
                        out.push(format!("key:{:?}", key.code).into_bytes());
                    }
                    _ => {}
                }
            }
            out
        };
        let _attach = raw(&mut rx);
        cx.simulate_keystrokes("cmd-left");
        cx.simulate_keystrokes("alt-backspace");
        cx.simulate_keystrokes("alt-right");
        assert_eq!(raw(&mut rx), [b"\x01".to_vec(), b"\x1b\x7f".to_vec(), b"\x1bf".to_vec()]);
        // The phone's key bar: ⌘ armed, then ⌫ from the bar.
        view.update(cx, |view, cx| {
            view.set_sticky_command(true, cx);
            view.press(Keystroke { key: "backspace".to_owned(), ..Keystroke::default() }, cx);
        });
        assert_eq!(raw(&mut rx), [b"\x15".to_vec()], "armed ⌘⌫ is ^U");
        assert!(!view.read_with(cx, |v, _| v.sticky_command()), "one key");

        view.update(cx, |view, cx| {
            let mut theme = Theme::default();
            theme.behaviour.natural_editing = false;
            view.set_theme(theme, cx);
        });
        cx.simulate_keystrokes("cmd-left");
        cx.simulate_keystrokes("alt-backspace");
        assert_eq!(raw(&mut rx), [b"key:Backspace".to_vec()], "off: ⌘← passes up, ⌥⌫ is a key");
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

    /// ⌘-press on a path and a pull past the slop drags the file out instead of opening it:
    /// the canvas hears `DragOut` with the path, and nothing is typed.
    #[gpui::test]
    fn cmd_drag_on_a_path_drags_the_file_out(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let dragged = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&dragged);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::DragOut { path } = event {
                    seen.borrow_mut().push(path.clone());
                }
            })
            .detach();
        });
        view.update_in(cx, |view, _window, cx| {
            view.apply(
                TermEvent::Frame(Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols: 30,
                    rows: 3,
                    cursor: Cursor::default(),
                    modes: TermModes::empty(),
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    images: Vec::new(),
                    updates: vec![RowUpdate {
                        row: 0,
                        line: Line::from_text("wrote out/report.pdf", 30, Style::DEFAULT),
                    }],
                }),
                cx,
            );
        });
        cx.run_until_parked();
        drain_words(&mut rx);
        let metrics = view.read_with(cx, |v, _| v.metrics.expect("laid out"));
        let at = metrics.origin + point(metrics.cell_width * 10.5, metrics.line_height * 0.5);
        let cmd = gpui::Modifiers { platform: true, ..gpui::Modifiers::default() };
        cx.simulate_mouse_down(at, MouseButton::Left, cmd);
        cx.simulate_mouse_move(at + point(px(2.0), px(0.0)), Some(MouseButton::Left), cmd);
        assert!(dragged.borrow().is_empty(), "within the slop it is still a click");
        cx.simulate_mouse_move(at + point(px(20.0), px(6.0)), Some(MouseButton::Left), cmd);
        cx.simulate_mouse_up(at + point(px(20.0), px(6.0)), MouseButton::Left, cmd);
        cx.run_until_parked();
        assert_eq!(*dragged.borrow(), ["out/report.pdf"]);
        assert_eq!(drain_words(&mut rx), Vec::<String>::new(), "a drag opens nothing");
    }

    /// ⌘-click on a path a compiler printed types the editor command at the prompt, with the
    /// line; a plain word nearby does nothing.
    #[gpui::test]
    fn cmd_click_on_a_path_opens_it_in_the_shells_editor(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update_in(cx, |view, _window, cx| {
            view.apply(
                TermEvent::Frame(Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols: 30,
                    rows: 3,
                    cursor: Cursor::default(),
                    modes: TermModes::empty(),
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    images: Vec::new(),
                    updates: vec![RowUpdate {
                        row: 0,
                        line: Line::from_text("error: src/main.rs:12:5 bad", 30, Style::DEFAULT),
                    }],
                }),
                cx,
            );
        });
        cx.run_until_parked();
        drain_words(&mut rx);
        let metrics = view.read_with(cx, |v, _| v.metrics.expect("laid out"));
        let cell = |col: u16| {
            metrics.origin
                + point(metrics.cell_width * (f32::from(col) + 0.5), metrics.line_height * 0.5)
        };
        let cmd = gpui::Modifiers { platform: true, ..gpui::Modifiers::default() };
        cx.simulate_click(cell(2), cmd);
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), Vec::<String>::new(), "a word is not a path");
        cx.simulate_click(cell(10), cmd);
        cx.run_until_parked();
        let mut pastes = Vec::new();
        let mut keys = 0_u32;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ClientMsg::Term { req: TermRequest::Paste(text), .. } => pastes.push(text),
                ClientMsg::Term { req: TermRequest::Key(_), .. } => {
                    keys = keys.saturating_add(1_u32);
                }
                _ => {}
            }
        }
        assert_eq!(pastes, ["${EDITOR:-vi} +12 'src/main.rs'"]);
        assert_eq!(keys, 1, "then ↩");
    }

    /// While a command runs there is no prompt to type at: ⌘-click on a path asks the canvas
    /// for a file card instead (`ViewFile` with the path as printed), and types nothing.
    #[gpui::test]
    fn cmd_click_on_a_path_while_a_command_runs_views_it(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let viewed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&viewed);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::ViewFile { path, line } = event {
                    seen.borrow_mut().push((path.clone(), *line));
                }
            })
            .detach();
        });
        // A prompt whose command is running: the block head is `cargo build`, the cursor sits
        // in its output, so `command_running` holds.
        view.update_in(cx, |view, _window, cx| {
            view.apply(
                TermEvent::Frame(Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols: 30,
                    rows: 3,
                    cursor: Cursor { row: 2, ..Cursor::default() },
                    modes: TermModes::empty(),
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    images: Vec::new(),
                    updates: vec![
                        RowUpdate {
                            row: 0,
                            line: {
                                let mut l = Line::from_text("$ cargo build", 30, Style::DEFAULT);
                                l.mark = SemanticMark::Prompt { exit: None, input: Some(2) };
                                l
                            },
                        },
                        RowUpdate {
                            row: 1,
                            line: Line::from_text(
                                "error: src/main.rs:12:5 bad",
                                30,
                                Style::DEFAULT,
                            ),
                        },
                    ],
                }),
                cx,
            );
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.state.command_running()), "the build is running");
        drain_words(&mut rx);
        let metrics = view.read_with(cx, |v, _| v.metrics.expect("laid out"));
        let cell = metrics.origin + point(metrics.cell_width * 10.5, metrics.line_height * 1.5);
        let cmd = gpui::Modifiers { platform: true, ..gpui::Modifiers::default() };
        cx.simulate_click(cell, cmd);
        cx.run_until_parked();
        assert_eq!(*viewed.borrow(), [("src/main.rs".to_owned(), Some(12))]);
        assert_eq!(drain_words(&mut rx), Vec::<String>::new(), "nothing typed into the build");
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
                    images: Vec::new(),
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

    /// The key bar's ⌘ then a tap on a path at a prompt views the file as a card, not the
    /// editor: a phone has no comfortable `vi`.
    #[gpui::test]
    fn sticky_command_views_the_path_under_the_next_tap(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let viewed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&viewed);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::ViewFile { path, line } = event {
                    seen.borrow_mut().push((path.clone(), *line));
                }
            })
            .detach();
        });
        view.update_in(cx, |view, _window, cx| {
            view.apply(
                TermEvent::Frame(Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols: 30,
                    rows: 3,
                    cursor: Cursor::default(),
                    modes: TermModes::empty(),
                    oldest_line: LineIndex(0),
                    first_visible_line: LineIndex(0),
                    total_lines: 3,
                    input_ack: 0,
                    images: Vec::new(),
                    updates: vec![RowUpdate {
                        row: 0,
                        line: Line::from_text("error: src/main.rs:12:5 bad", 30, Style::DEFAULT),
                    }],
                }),
                cx,
            );
            view.set_sticky_command(true, cx);
        });
        cx.run_until_parked();
        drain_words(&mut rx);
        let metrics = view.read_with(cx, |v, _| v.metrics.expect("laid out"));
        let cell = metrics.origin + point(metrics.cell_width * 10.5, metrics.line_height * 0.5);
        cx.simulate_click(cell, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*viewed.borrow(), [("src/main.rs".to_owned(), Some(12))]);
        assert_eq!(drain_words(&mut rx), Vec::<String>::new(), "nothing typed at the prompt");
        assert!(!view.read_with(cx, |v, _| v.sticky_command()), "one tap");
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
                    images: Vec::new(),
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

    /// The screen of [`with_command_blocks`] again, the cursor showing after the prompt.
    fn cursor_at_the_prompt(view: &Entity<TerminalView>, cx: &mut VisualTestContext) {
        let prompt = SemanticMark::Prompt { exit: Some(0), input: Some(2) };
        let cursor = Cursor { row: 2, col: 2, visible: true, ..Cursor::default() };
        view.update_in(cx, |view, _window, cx| {
            let rows = ["2".to_owned(), String::new(), "$ ".to_owned()];
            let TermEvent::Frame(mut frame) = screen_of(2, 10, &rows) else { panic!("a frame") };
            frame.first_visible_line = LineIndex(6);
            frame.total_lines = 9;
            frame.cursor = cursor;
            if let Some(last) = frame.updates.last_mut() {
                last.line.mark = prompt;
            }
            view.apply(TermEvent::Frame(frame), cx);
        });
        cx.run_until_parked();
    }

    /// An auto-repeated key goes the way a first press does: to the predictor (it echoes
    /// locally), to the latency meter, and back to the bottom of the history.
    #[gpui::test]
    fn a_held_key_is_predicted_timed_and_follows_the_output(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        with_command_blocks(&view, cx);
        cursor_at_the_prompt(&view, cx);
        view.update(cx, |view, _cx| {
            view.predictor.set_policy(Policy::Always);
            let _effects = view.state.scroll_to(3);
        });
        assert_eq!(view.read_with(cx, |v, _| v.state.view_offset()), 3, "scrolled up");
        let keystroke =
            Keystroke { key: "a".into(), key_char: Some("a".into()), ..Keystroke::default() };
        cx.simulate_event(KeyDownEvent { keystroke, is_held: true, prefer_character_input: false });
        view.read_with(cx, |view, _| {
            assert_eq!(view.state.view_offset(), 0, "back at the bottom");
            assert_eq!(view.predictor.pending().len(), 1, "guessed");
            assert!(view.latency.waiting(), "timed");
        });
        assert_eq!(drain_words(&mut rx), ["key"], "and sent");
    }

    /// The meter reads a frame when it is presented — at the next frame callback — not when
    /// it is painted: the host's echo drawn now counts only once the display takes it.
    #[gpui::test]
    fn a_key_is_timed_when_its_frame_is_presented(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        cx.simulate_keystrokes("a");
        let seq = view.read_with(cx, |v, _| v.key_seq);
        view.update_in(cx, |view, _window, cx| {
            let TermEvent::Frame(mut frame) = screen_of(1, 10, &["a".to_owned()]) else {
                panic!("a frame")
            };
            frame.input_ack = seq;
            view.apply(TermEvent::Frame(frame), cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.latency().echoed), 0, "painted, not yet shown");
        let ran = cx.update(Window::simulate_next_frame);
        assert!(ran >= 1, "the paint asked for the next frame");
        assert_eq!(view.read_with(cx, |v, _| v.latency().echoed), 1, "presented: timed");
        assert!(!view.read_with(cx, |v, _| v.latency_waiting()), "nothing left to time");
    }

    /// Box drawing is masked once per character and cell size and painted from the atlas on
    /// every later frame; while the zoom is in motion no mask is written for the passing sizes.
    #[gpui::test]
    fn a_sprite_is_masked_once_and_not_while_zooming(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        let masks = |cx: &mut VisualTestContext| {
            cx.update(|_window, cx| crate::terminal::element::sprite_masks(cx))
        };
        let boxed = ["╭──╮".to_owned(), "│  │".to_owned(), "╰──╯".to_owned()];
        view.update_in(cx, |view, _window, cx| view.apply(screen_of(1, 10, &boxed), cx));
        cx.run_until_parked();
        let first = masks(cx);
        assert_eq!(first, 6, "╭ ─ ╮ │ ╰ ╯");
        let again = ["╰──╯".to_owned(), "╭──╮".to_owned(), "││││".to_owned()];
        view.update_in(cx, |view, _window, cx| view.apply(screen_of(2, 10, &again), cx));
        cx.run_until_parked();
        assert_eq!(masks(cx), first, "the same characters in the same cell: no new mask");
        for (seq, zoom) in [(3, 1.3), (4, 1.7)] {
            view.update_in(cx, |view, _window, cx| {
                view.set_zoom(zoom);
                view.set_zooming(true);
                view.apply(screen_of(seq, 10, &boxed), cx);
            });
            cx.run_until_parked();
        }
        assert_eq!(masks(cx), first, "in motion the geometry is painted instead");
    }

    /// `rows` rows of lowercase words, about `width` columns each, from `seed`.
    fn prose(mut seed: u32, rows: usize, width: usize) -> Vec<String> {
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        std::iter::repeat_with(|| {
            let mut row = String::new();
            while row.len() < width {
                for _ in 0..next().wrapping_rem(6).wrapping_add(3) {
                    row.push(char::from(b'a'.wrapping_add(u8::try_from(next() % 26).unwrap())));
                }
                row.push(' ');
            }
            row
        })
        .take(rows)
        .collect()
    }

    /// A full frame of `text`, one row each, `cols` wide.
    fn screen_of(seq: u64, cols: u16, text: &[String]) -> TermEvent {
        let rows = u16::try_from(text.len()).unwrap();
        TermEvent::Frame(Frame {
            seq,
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
            images: Vec::new(),
            updates: text
                .iter()
                .enumerate()
                .map(|(row, text)| RowUpdate {
                    row: u16::try_from(row).unwrap(),
                    line: Line::from_text(text, cols, Style::DEFAULT),
                })
                .collect(),
        })
    }

    /// A 100 × 40 grid of prose is shown, then another for three frames, then the first again:
    /// what a pan back or a scroll back does. Nothing is shaped on the return — the word cache
    /// forgets by a budget, not by frames unseen. Prints the numbers MEASUREMENTS records.
    #[gpui::test]
    fn a_screen_shown_again_shapes_nothing(cx: &mut TestAppContext) {
        let (tx, _rx) = mpsc::channel(4096);
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let size = TermSize { cols: 100, rows: 40, ..TermSize::default() };
            let view = TerminalView::new(SessionId::new(), size, tx, Theme::default(), cx);
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(1000.0), px(900.0)));
        cx.run_until_parked();
        let (a, b) = (prose(0x2545_f491, 40, 90), prose(0x9e37_79b9, 40, 90));
        let shaped = |cx: &mut VisualTestContext| {
            cx.update(|_window, cx| crate::terminal::element::shaped_words(cx))
        };
        let draw = |seq: u64, rows: &[String], cx: &mut VisualTestContext| {
            let started = Instant::now();
            view.update_in(cx, |view, _window, cx| view.apply(screen_of(seq, 100, rows), cx));
            cx.run_until_parked();
            started.elapsed()
        };
        let first = draw(1, &a, cx);
        let mut steady = Duration::ZERO;
        for seq in 2..=4 {
            steady = steady.max(draw(seq, &b, cx));
        }
        let before = shaped(cx);
        let back = draw(5, &a, cx);
        let reshaped = shaped(cx).saturating_sub(before);
        println!(
            "MEASURE word cache: first {first:?}, steady max {steady:?}, return {back:?}, \
             words reshaped on the return {reshaped}"
        );
        assert_eq!(reshaped, 0, "the first screen is still shaped");
    }

    /// Every message the host received that types or clears, oldest first, as one word each.
    fn drain_words(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(match msg {
                ClientMsg::Term { req: TermRequest::Key(_), .. } => "key".to_owned(),
                ClientMsg::Term { req: TermRequest::Paste(_), .. } => "paste".to_owned(),
                ClientMsg::Term { req: TermRequest::Clear, .. } => "clear".to_owned(),
                // Resizes and the like: not what these tests are about.
                _ => continue,
            });
        }
        out
    }
}
