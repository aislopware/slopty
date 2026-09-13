//! `TerminalView`: one attached session on screen.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Autocapitalize, Bounds, Context, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent,
    Keystroke, LongPressEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, TextInputAction, TextInputConfiguration,
    TouchPhase, UTF16Selection, Window, anchored, deferred, div, point, px, size,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::term::{BlockHead, CommandBlock};
use slopty_client::{Effect, TermState};
use slopty_core::SessionId;
use slopty_grid::{Cursor, LineIndex, TermModes};
use slopty_predict::{Policy, Prediction, Predictor};
use slopty_proto::ClientMsg;
use slopty_proto::agent::{
    AgentInfo, AgentSet, AgentStatus, AgentTask, BlockReason, PermissionRequest, QuestionAnswer,
    ToolDetail, TranscriptFollow, TranscriptUpdate,
};
use slopty_proto::input::{MouseAction, MouseButton as ProtoButton, MouseEvent};
use slopty_proto::terminal::{SearchMatch, TermEvent, TermRequest, TermSize};
use slopty_theme::{Theme, alpha};
use tokio::sync::mpsc;

use crate::colors::{hsla, hsla_alpha};
use crate::keys;
use crate::terminal::conversation::{self, Attention, Conversation};
use crate::terminal::element::{CellMetrics, TerminalElement, separator_color};
use crate::terminal::{latency, url};

/// Hits asked for per search; the host counts every hit regardless.
const SEARCH_MAX: u32 = 5_000;
/// While the search bar is open, output refreshes the hits at most this often.
const SEARCH_REFRESH: Duration = Duration::from_millis(300);
/// How often a selection dragged past the grid's edge scrolls, and the most lines one tick
/// moves (the pointer's distance past the edge picks the pace, one line per row of distance).
const AUTOSCROLL_TICK: Duration = Duration::from_millis(50);
const AUTOSCROLL_MAX: i64 = 8;

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
            /// Copy the whole conversation as Markdown.
            CopyConversation,
            /// Run the last command again: a paste of what was typed, then ↩.
            RerunLast,
            /// Save the last command and its output as a note card beside the shell.
            NoteLastBlock,
            /// Clear the screen and the history (⌘K, as in every Mac terminal).
            ClearScreen,
            /// Show the agent's conversation instead of the grid, or the grid again.
            ToggleConversation,
            /// Tab in a driven composer with the slash list up: take the selected
            /// completion. Bound in the Terminal context so it is tried before gpui-kit's
            /// root `Tab` (which moves the focus ring); it propagates when there is
            /// nothing to complete.
            CompleteSlash,
        ]
    );
}
pub use actions::{
    ClearScreen, CloseFind, CompleteSlash, Copy, CopyConversation, CopyLastOutput, Find, FindNext,
    FindPrev, NextPrompt, NoteLastBlock, Paste, PrevPrompt, RerunLast, ToggleConversation,
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
        KeyBinding::new("cmd-shift-enter", RerunLast, CTX),
        KeyBinding::new("cmd-k", ClearScreen, CTX),
        KeyBinding::new("cmd-shift-l", ToggleConversation, CTX),
        KeyBinding::new("tab", CompleteSlash, CTX),
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
    /// A driven session's Allow / Deny row was pressed, or its question answered: the
    /// canvas answers the host's permission request by id.
    AgentAnswered {
        /// The request id (`PermissionRequest::id`).
        request: String,
        /// Allow or deny.
        allowed: bool,
        /// The answers when the request was a question; empty otherwise.
        answers: Vec<QuestionAnswer>,
        /// Allow and take the agent's suggestion for not asking again.
        always: bool,
    },
    /// Something the human should read in the top bar for a moment (a picture refused).
    Notice(String),
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
    /// The "run" button on a fenced block in an answer was pressed: the canvas reveals the
    /// shell it picked and types the code into it. The payload is the block's body, lines
    /// joined with `\n`.
    RunInShell(String),
    /// "Ask the agent" on a block's menu: the block as a Markdown fence for a driven agent's
    /// composer. The canvas picks the agent card (or opens one) and puts it there.
    AskAgent(String),
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
    /// The selection is being dragged past the grid's top or bottom: lines to scroll each
    /// tick (positive = up into history) and the column the pointer holds.
    autoscroll: Option<(i64, u16)>,
    /// The ticking loop behind `autoscroll`; dropped (cancelled) when a new drag starts one.
    autoscroll_task: Option<gpui::Task<()>>,
    /// The scrollbar's thumb is held: the pointer's offset from the thumb's top.
    thumb_drag: Option<Pixels>,
    /// The fraction of a line the wheel has moved short of a whole one (a trackpad scrolls
    /// in fractions; they add up).
    wheel_remainder: f32,
    /// The command-block menu a right click opened, and where.
    block_menu: Option<BlockMenu>,
    /// The host's answer to the composer's `@` word: the query asked and the paths found.
    files: Option<(String, Vec<String>)>,
    /// The `@` query last asked of the host, so a keystroke that leaves it alone asks nothing.
    files_asked: Option<String>,
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
    /// The host drives this session's agent over its structured protocol: the conversation
    /// is the whole view (there is no grid), the composer speaks to the agent, and the answers
    /// go back by request id.
    driven: bool,
    /// Open the conversation on the next frame (a driven view is born without a window).
    open_conversation: bool,
    /// Text for the composer once the conversation exists (a driven view opens it on its
    /// first frame, so a block asked of a card that was just opened has to wait).
    pending_compose: Option<String>,
    /// A window whose picture waits for the conversation to exist before it is attached.
    pending_snapshot: Option<(slopty_proto::screen::CaptureTarget, String)>,
    /// The permission the driven agent waits on, with the input it would run with.
    permission: Option<PermissionRequest>,
    /// The labels picked so far for each question of a pending `AskUserQuestion`.
    chosen: Vec<Vec<String>>,
    /// Pictures pasted into the composer, going with the next prompt.
    attachments: Vec<slopty_proto::agent::Image>,
    /// Host windows or displays whose picture the host attaches as the next prompt goes,
    /// with their titles for the chips.
    snapshots: Vec<(slopty_proto::screen::CaptureTarget, String)>,
    /// Pictures being made fit off the UI thread, not yet in `attachments`.
    preparing: usize,
    /// The text the driven agent is writing now, ahead of its next entry.
    partial: String,
    /// What the driven agent said about itself (model, mode, slash commands, turns, cost).
    info: AgentInfo,
    /// The canvas has a plain shell to run a fenced block in: the "run" button beside "copy"
    /// is drawn. Set by the canvas whenever its set of shells changes, so the conversation's
    /// render stays a pure function of the view's own state.
    can_run_in_shell: bool,
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
            block_menu: None,
            files: None,
            files_asked: None,
            command_started: None,
            took: HashMap::new(),
            took_epoch: None,
            selection: None,
            selecting: false,
            hover: None,
            cmd_held: false,
            touch_selecting: false,
            autoscroll: None,
            autoscroll_task: None,
            thumb_drag: None,
            wheel_remainder: 0.0,
            search: None,
            conversation: None,
            agent: None,
            answered: None,
            focus_composer: false,
            search_regex: false,
            driven: false,
            chosen: Vec::new(),
            attachments: Vec::new(),
            snapshots: Vec::new(),
            preparing: 0,
            open_conversation: false,
            pending_compose: None,
            pending_snapshot: None,
            permission: None,
            partial: String::new(),
            info: AgentInfo::default(),
            can_run_in_shell: false,
        }
    }

    /// Make this the view of a session the host drives (`SessionKind::Agent`): the
    /// conversation opens on the first frame and stays; there is nothing else to show.
    pub fn set_driven(&mut self, cx: &mut Context<Self>) {
        self.driven = true;
        self.open_conversation = true;
        cx.notify();
    }

    /// Put `text` into the conversation's composer, now or as soon as the conversation
    /// exists.
    pub fn compose(&mut self, text: String, cx: &mut Context<Self>) {
        self.pending_compose = Some(text);
        cx.notify();
    }

    /// Whether the host drives this session's agent (the conversation is the whole view).
    #[must_use]
    pub const fn is_driven(&self) -> bool {
        self.driven
    }

    /// The canvas says whether it has a plain shell to run a fenced block in: while it has
    /// none, the "run" button beside "copy" is not drawn, because there is nowhere to send
    /// the code.
    pub fn set_can_run_in_shell(&mut self, can: bool, cx: &mut Context<Self>) {
        if self.can_run_in_shell == can {
            return;
        }
        self.can_run_in_shell = can;
        cx.notify();
    }

    /// Whether the canvas has a shell for the "run" button.
    #[must_use]
    pub const fn can_run_in_shell(&self) -> bool {
        self.can_run_in_shell
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

    /// The text the driven agent is writing now.
    #[must_use]
    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// The permission the driven agent waits on.
    #[must_use]
    pub const fn permission(&self) -> Option<&PermissionRequest> {
        self.permission.as_ref()
    }

    /// A slice of what the driven agent is writing, replacing the last one; empty once the
    /// text became an entry.
    pub fn agent_partial(&mut self, text: String, cx: &mut Context<Self>) {
        if self.partial != text {
            self.partial = text;
            cx.notify();
        }
    }

    /// The driven agent asks to run a tool; shown above the composer with Allow / Deny, or
    /// as the question's options when the tool is `AskUserQuestion`.
    pub fn agent_permission(&mut self, request: PermissionRequest, cx: &mut Context<Self>) {
        self.chosen = match &request.detail {
            ToolDetail::Question { questions } => vec![Vec::new(); questions.len()],
            _ => Vec::new(),
        };
        self.permission = Some(request);
        self.answered = None;
        cx.notify();
    }

    /// An option of a pending question was tapped: picked (or, for a multi-select question,
    /// toggled). When every question has its pick and none is multi-select, the answer goes
    /// out at once — one tap is the whole exchange; otherwise the Answer button sends it.
    pub fn choose(&mut self, question: usize, label: &str, cx: &mut Context<Self>) {
        if self.answered.is_some() {
            return;
        }
        let Some(ToolDetail::Question { questions }) = self.permission.as_ref().map(|p| &p.detail)
        else {
            return;
        };
        let Some(q) = questions.get(question) else { return };
        let multi = q.multi;
        let all_single = questions.iter().all(|q| !q.multi);
        let Some(picked) = self.chosen.get_mut(question) else { return };
        if multi {
            if let Some(ix) = picked.iter().position(|l| l == label) {
                picked.remove(ix);
            } else {
                picked.push(label.to_owned());
            }
        } else {
            *picked = vec![label.to_owned()];
        }
        if all_single && self.chosen.iter().all(|c| !c.is_empty()) {
            self.answer_question(cx);
        } else {
            cx.notify();
        }
    }

    /// Send the pending question's answers, when every question has one: each question's
    /// picks joined with ", ", under the question's own text.
    pub fn answer_question(&mut self, cx: &mut Context<Self>) {
        if self.answered.is_some() {
            return;
        }
        let Some(request) = self.permission.as_ref() else { return };
        let ToolDetail::Question { questions } = &request.detail else { return };
        if questions.len() != self.chosen.len() || self.chosen.iter().any(Vec::is_empty) {
            cx.notify();
            return;
        }
        let answers = questions
            .iter()
            .zip(&self.chosen)
            .map(|(q, picks)| QuestionAnswer { question: q.text.clone(), answer: picks.join(", ") })
            .collect();
        let request = request.id.clone();
        self.answered = Some(true);
        cx.emit(TerminalViewEvent::AgentAnswered {
            request,
            allowed: true,
            answers,
            always: false,
        });
        cx.notify();
    }

    /// Typed text as the answer to the first question still without one; `false` when no
    /// question is pending.
    fn answer_question_with(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        let pending = self.answered.is_none()
            && matches!(
                self.permission.as_ref().map(|p| &p.detail),
                Some(ToolDetail::Question { .. })
            );
        if !pending {
            return false;
        }
        if let Some(picks) = self.chosen.iter_mut().find(|c| c.is_empty()) {
            picks.push(text);
        }
        self.answer_question(cx);
        true
    }

    /// The labels picked so far, per question of the pending `AskUserQuestion`.
    #[must_use]
    pub fn chosen(&self) -> &[Vec<String>] {
        &self.chosen
    }

    /// What the driven agent says about itself, whole.
    pub fn agent_info(&mut self, info: AgentInfo, cx: &mut Context<Self>) {
        if self.info != info {
            self.info = info;
            cx.notify();
        }
    }

    /// A subagent the driven agent spawned: shown under the call that spawned it.
    pub fn agent_task(&mut self, task: AgentTask, cx: &mut Context<Self>) {
        if let Some(conversation) = &mut self.conversation {
            conversation.set_task(task);
            cx.notify();
        }
    }

    /// "View" on a tool call: a file card for its path, made absolute against the agent's
    /// working directory when the agent gave it relative (Claude Code's tools take absolute
    /// paths, but a fake or a hook may not).
    pub fn view_file(&self, path: &str, line: Option<u32>, cx: &mut Context<Self>) {
        let absolute = if path.starts_with('/') {
            path.to_owned()
        } else {
            match &self.info.cwd {
                Some(cwd) => format!("{}/{path}", cwd.trim_end_matches('/')),
                None => path.to_owned(),
            }
        };
        cx.emit(TerminalViewEvent::ViewFile { path: absolute, line });
    }

    /// What the driven agent last said about itself.
    #[must_use]
    pub const fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// What completes the composer's text right now, as Tab would insert it: slash
    /// commands by name, paths as `@path`.
    #[must_use]
    pub fn completions(&self, cx: &gpui::App) -> Vec<String> {
        if !self.driven {
            return Vec::new();
        }
        self.conversation
            .as_ref()
            .map(|c| c.completions(&self.info.slash_commands, self.files.as_ref(), cx))
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.insert)
            .collect()
    }

    /// The host's answer to a `ListFiles`: kept when its query is still the `@` word the
    /// composer ends in, dropped as stale otherwise.
    pub fn files(&mut self, query: String, paths: Vec<String>, cx: &mut Context<Self>) {
        let current = self
            .conversation
            .as_ref()
            .and_then(|c| conversation::file_query(&c.composer_text(cx)).map(str::to_owned));
        if current.as_deref() == Some(query.as_str()) {
            self.files = Some((query, paths));
            cx.notify();
        }
    }

    /// The model chip: open or close the menu of models.
    pub fn toggle_model_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.conversation.as_mut() {
            c.set_model_menu(!c.model_menu_open());
            cx.notify();
        }
    }

    /// A model from the menu: ask the host to switch the agent to it; the header changes
    /// when the agent confirms.
    pub fn set_model(&mut self, model: &str, cx: &mut Context<Self>) {
        if let Some(c) = self.conversation.as_mut() {
            c.set_model_menu(false);
        }
        self.say(ClientMsg::AgentSet(AgentSet {
            session: self.session,
            model: Some(model.to_owned()),
            permission_mode: None,
        }));
        cx.notify();
    }

    /// The mode chip: ask the host for the next permission mode.
    pub fn cycle_permission_mode(&self, cx: &mut Context<Self>) {
        let next =
            conversation::next_mode(self.info.permission_mode.as_deref().unwrap_or("default"));
        self.say(ClientMsg::AgentSet(AgentSet {
            session: self.session,
            model: None,
            permission_mode: Some(next.to_owned()),
        }));
        cx.notify();
    }

    /// The composer's text changed: the completion list starts over from its first match,
    /// and an `@` word the text now ends in is asked of the host (once per query).
    pub fn composer_changed(&mut self, cx: &mut Context<Self>) {
        let query = self
            .conversation
            .as_ref()
            .and_then(|c| conversation::file_query(&c.composer_text(cx)).map(str::to_owned))
            .filter(|q| !q.is_empty());
        if let Some(c) = self.conversation.as_mut() {
            c.composer_changed();
        }
        if query != self.files_asked {
            self.files_asked.clone_from(&query);
            if let Some(query) = query {
                self.say(ClientMsg::ListFiles { session: self.session, query });
            } else {
                self.files = None;
            }
        }
        cx.notify();
    }

    /// Keys for the slash-completion list and the model menu, taken in the capture phase so
    /// the composer's own bindings (↑/↓ move its caret, Esc clears it) never see them while
    /// the list is up: Tab takes the selected completion, ↑/↓ choose, Esc hides the list or
    /// closes the menu.
    fn completion_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &event.keystroke;
        if !k.modifiers.modified() && self.completion_nav(&k.key, window, cx) {
            cx.stop_propagation();
        }
    }

    /// The composer's own ↑ / ↓ / Esc actions arrive before any key event; while the
    /// completion list or the model menu is up they belong to it, so they are caught in the
    /// capture phase and stopped there.
    fn composer_action(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.completion_nav(key, window, cx) {
            cx.stop_propagation();
            return;
        }
        // ↑ / ↓ with nothing to complete: a sent prompt back, as a shell's history.
        let delta = match key {
            "up" => -1,
            "down" => 1,
            _ => return,
        };
        if self.driven
            && self.composer_focused(window, cx)
            && self.conversation.as_mut().is_some_and(|c| c.recall(delta, window, cx))
        {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// `key` as the completion list or the model menu takes it; `false` when neither is up
    /// or the key is not theirs.
    fn completion_nav(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.driven || !self.composer_focused(window, cx) {
            return false;
        }
        if key == "escape" && self.conversation.as_ref().is_some_and(Conversation::model_menu_open)
        {
            self.toggle_model_menu(cx);
            return true;
        }
        let completions = self.completions(cx);
        if completions.is_empty() {
            return false;
        }
        let handled = match key {
            "tab" => {
                let at = self.conversation.as_ref().map_or(0, Conversation::selected_completion);
                if let Some(insert) =
                    completions.get(at.min(completions.len().saturating_sub(1))).cloned()
                {
                    self.complete_with(&insert, window, cx);
                }
                true
            }
            "down" | "up" => {
                let delta = if key == "down" { 1 } else { -1 };
                if let Some(c) = self.conversation.as_mut() {
                    c.step_completion(delta, completions.len());
                }
                true
            }
            "escape" => {
                if let Some(c) = self.conversation.as_mut() {
                    c.hide_completions();
                }
                true
            }
            _ => false,
        };
        if handled {
            cx.notify();
        }
        handled
    }

    /// Put a completion in the composer (Tab, or a click on one): a slash command, or an
    /// `@path` in place of the `@` word.
    pub fn complete_with(&mut self, insert: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = self.conversation.as_mut() {
            c.complete(insert, window, cx);
            c.focus_composer(window, cx);
            cx.notify();
        }
    }

    fn say(&self, msg: ClientMsg) {
        if let Err(e) = self.out.try_send(msg) {
            tracing::warn!(session = %self.session, error = %e, "outbound queue");
        }
    }

    /// Esc or ⌃C in a driven conversation: stop the agent's turn.
    pub fn interrupt_agent(&self) {
        self.say(ClientMsg::AgentInterrupt { session: self.session });
    }

    /// ⌘⇧L or the title-bar pill: show the agent's conversation instead of the grid, or the
    /// grid again. The host is told to start or stop tailing the transcript. The composer
    /// takes the keyboard with the conversation; the grid takes it back.
    pub fn toggle_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.driven {
            // The conversation is the whole view.
            return;
        }
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
        let Some(conversation) = self.conversation.as_mut() else { return };
        let text = conversation.take_composer_text(window, cx);
        if self.driven {
            if text.trim().is_empty() && self.attachments.is_empty() && self.snapshots.is_empty() {
                return;
            }
            // While the agent asks a question, the typed text is its answer ("Other"), not
            // a new prompt; the pictures wait for the next prompt.
            if text.trim().is_empty() || !self.answer_question_with(text.clone(), cx) {
                let images = std::mem::take(&mut self.attachments);
                let snapshots =
                    std::mem::take(&mut self.snapshots).into_iter().map(|(t, _)| t).collect();
                // Sent mid-turn, the prompt waits in the agent's queue: shown as queued here.
                let queued = (self.agent_busy() && !text.trim().is_empty()).then(|| text.clone());
                self.say(ClientMsg::AgentSay { session: self.session, text, images, snapshots });
                if let Some(text) = queued
                    && let Some(conversation) = self.conversation.as_mut()
                {
                    conversation.queue(text);
                }
                cx.notify();
            }
            return;
        }
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
        if self.driven {
            let Some(request) = self.permission.as_ref().map(|p| p.id.clone()) else { return };
            self.answered = Some(allowed);
            cx.emit(TerminalViewEvent::AgentAnswered {
                request,
                allowed,
                answers: Vec::new(),
                always: false,
            });
            cx.notify();
            return;
        }
        self.answered = Some(allowed);
        cx.emit(TerminalViewEvent::Answered { allowed });
        cx.notify();
    }

    /// The conversation's Always: allow, and take the agent's suggestion for not asking
    /// again (`PermissionRequest::always`). Only a driven view has one.
    pub fn answer_always(&mut self, cx: &mut Context<Self>) {
        if self.answered.is_some() || !self.driven {
            return;
        }
        let Some(request) = self.permission.as_ref().filter(|p| p.always.is_some()) else {
            return;
        };
        let request = request.id.clone();
        self.answered = Some(true);
        cx.emit(TerminalViewEvent::AgentAnswered {
            request,
            allowed: true,
            answers: Vec::new(),
            always: true,
        });
        cx.notify();
    }

    /// The host's word on the agent in this session (`None`: no agent). A question or an
    /// elicitation puts the caret in the composer when the conversation is on.
    pub fn set_agent_status(&mut self, status: Option<AgentStatus>, cx: &mut Context<Self>) {
        self.answered = None;
        if !matches!(
            status,
            Some(AgentStatus::Blocked(BlockReason::Permission { .. } | BlockReason::Question))
        ) {
            self.permission = None;
            self.chosen.clear();
        }
        if self.conversation.is_some()
            && matches!(
                status,
                Some(AgentStatus::Blocked(BlockReason::Question | BlockReason::Elicitation))
            )
        {
            self.focus_composer = true;
        }
        self.agent = status;
        // A turn that ended, or an agent gone: nothing of what was queued waits any more
        // (a prompt the agent took starts a turn of its own and arrives as an entry).
        if !self.agent_busy()
            && !matches!(self.agent, Some(AgentStatus::Blocked(_)))
            && let Some(conversation) = self.conversation.as_mut()
        {
            conversation.clear_queued();
        }
        cx.notify();
    }

    /// Whether the agent is in a turn: working, or in a tool.
    const fn agent_busy(&self) -> bool {
        matches!(self.agent, Some(AgentStatus::Working | AgentStatus::Tool { .. }))
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
            AgentStatus::Blocked(BlockReason::Permission { tool }) => Some(Attention::Permission {
                tool: tool.clone(),
                answered: self.answered,
                detail: self.permission.as_ref().map(|p| p.summary.clone()),
                always: self.permission.as_ref().and_then(|p| p.always.clone()),
            }),
            AgentStatus::Blocked(BlockReason::Question) => match &self.permission {
                Some(PermissionRequest { detail: ToolDetail::Question { questions }, .. }) => {
                    Some(Attention::Question {
                        questions: questions.clone(),
                        chosen: self.chosen.clone(),
                        answered: self.answered.is_some(),
                    })
                }
                _ => Some(Attention::Prompt),
            },
            AgentStatus::Blocked(BlockReason::Elicitation) => Some(Attention::Prompt),
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

    /// A click on a sent prompt: it is in the composer again, to edit and send.
    pub fn reuse_prompt(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = &mut self.conversation
            && conversation.reuse(ix, window, cx)
        {
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

    /// The conversation on show, to change.
    #[must_use]
    pub const fn conversation_mut(&mut self) -> Option<&mut Conversation> {
        self.conversation.as_mut()
    }

    /// A slice of the transcript from the host; ignored once the conversation is hidden.
    pub fn transcript_update(&mut self, update: TranscriptUpdate, cx: &mut Context<Self>) {
        if let Some(conversation) = &mut self.conversation {
            conversation.apply(update);
            // An open find bar keeps its hits true to the transcript, staying on its entry.
            let keep = self
                .search
                .as_ref()
                .and_then(|s| s.current.and_then(|c| conversation.hits().0.get(c).copied()));
            if self.search.is_some() {
                self.search_conversation(Some(keep.unwrap_or(usize::MAX)), cx);
            }
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
            match &mut self.conversation {
                Some(conversation) => {
                    conversation.set_hits(Vec::new(), None);
                    conversation.focus_composer(window, cx);
                }
                None => self.focus.focus(window, cx),
            }
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
        if self.conversation.is_some() {
            self.search_conversation(None, cx);
        } else {
            self.send_search(cx);
        }
    }

    /// Find the needle in the conversation's entries here, no host involved: the hits land
    /// on the newest (`keep`: the entry to stay on when it is still a hit, once the
    /// transcript grew under an open bar), and the list scrolls to it.
    fn search_conversation(&mut self, keep: Option<usize>, cx: &mut Context<Self>) {
        let (Some(search), Some(conversation)) = (&mut self.search, &mut self.conversation) else {
            return;
        };
        match conversation::entry_hits(conversation.entries(), &search.needle, search.regex) {
            Ok(hits) => {
                search.total = u32::try_from(hits.len()).unwrap_or(u32::MAX);
                search.current = keep
                    .and_then(|entry| hits.iter().position(|&h| h == entry))
                    .or_else(|| hits.len().checked_sub(1));
                search.invalid = None;
                conversation.set_hits(hits, search.current);
            }
            Err(message) => {
                search.total = 0;
                search.current = None;
                search.invalid = Some(message);
                conversation.set_hits(Vec::new(), None);
            }
        }
        if keep.is_none() {
            self.reveal_current(cx);
        }
        cx.notify();
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
        let n = self.conversation.as_ref().map_or(search.matches.len(), |c| c.hits().0.len());
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
        if let (Some(search), Some(conversation)) = (&self.search, &mut self.conversation) {
            let hit = search.current.and_then(|c| conversation.hits().0.get(c).copied());
            conversation.set_hits(conversation.hits().0.to_vec(), search.current);
            if let Some(ix) = hit {
                conversation.reveal_entry(ix);
            }
            cx.notify();
            return;
        }
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

    /// The block menu's items, in order: what applies to this block.
    fn block_menu_items(block: &CommandBlock) -> Vec<BlockMenuItem> {
        let mut items = Vec::new();
        if block.command.is_some() {
            items.push(BlockMenuItem::CopyCommand);
        }
        if !block.output.is_empty() {
            items.push(BlockMenuItem::CopyOutput);
        }
        if block.command.is_some() {
            items.push(BlockMenuItem::Rerun);
        }
        items.push(BlockMenuItem::Ask);
        items.push(BlockMenuItem::Note);
        items.push(BlockMenuItem::SelectBlock);
        items
    }

    /// A block menu item was chosen: do it and close the menu.
    fn block_menu_pick(&mut self, item: BlockMenuItem, cx: &mut Context<Self>) {
        let Some(menu) = self.block_menu.take() else { return };
        let block = menu.block;
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
            BlockMenuItem::Ask => {
                // A selection is what the human meant; the block is the default.
                let text = self
                    .selected_text()
                    .filter(|t| !t.trim().is_empty())
                    .map_or_else(|| block_markdown(&block), |t| format!("```\n{t}\n```\n\n"));
                cx.emit(TerminalViewEvent::AskAgent(text));
            }
            BlockMenuItem::Note => cx.emit(TerminalViewEvent::NoteBlock(block_note(&block))),
            BlockMenuItem::SelectBlock => {
                let last = LineIndex(block.end.0.saturating_sub(1).max(block.prompt.0));
                let cols = self.state.size().cols;
                self.selection = Some(Selection {
                    anchor: (block.prompt, 0),
                    head: (last, cols.saturating_sub(1)),
                });
                self.selecting = false;
            }
        }
        cx.notify();
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
        let family = theme.typography.mono_families.first().cloned().unwrap_or_default();
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
                .child(command)
                .into_any_element(),
        )
    }

    fn render_block_menu(&self, menu: &BlockMenu, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = &theme.spacing;
        let items = Self::block_menu_items(&menu.block);
        let list = div()
            .id("block-menu")
            .debug_selector(|| "block-menu".to_owned())
            .role(gpui::accesskit::Role::Menu)
            .aria_label("Command block")
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(160.0))
            .p(px(spacing.xs))
            .rounded(px(theme.radii.sm))
            .border_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .shadow_md()
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
                    .hover(move |el| el.bg(hsla_alpha(s.accent, alpha::TINT_PRESSED)))
                    .child(SharedString::from(item.label()));
                crate::a11y::tab_stop(row, s.accent).on_click(cx.listener(
                    move |this, _ev, _window, cx| {
                        this.block_menu_pick(item, cx);
                    },
                ))
            }));
        deferred(anchored().position(menu.at).snap_to_window_with_margin(px(8.0)).child(list))
            .with_priority(1)
            .into_any_element()
    }

    /// ⌘⇧C: the last command's output (shell integration marks it) to the clipboard; in a
    /// conversation, the newest answer's Markdown.
    pub fn copy_last_output(
        &mut self,
        _: &CopyLastOutput,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match &self.conversation {
            Some(conversation) => conversation.last_answer().map(str::to_owned),
            None => self.state.last_command_output(),
        };
        if let Some(text) = text {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    /// The palette's "Copy conversation as Markdown": every turn, call and notice of the
    /// conversation on show (nothing in a shell).
    pub fn copy_conversation(
        &mut self,
        _: &CopyConversation,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(conversation) = &self.conversation {
            let text = conversation::as_markdown(conversation.entries());
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

    /// ⌘↑: the prompt above the viewport's top row, scrolled to the top. In a conversation
    /// the prompts are the `User` entries.
    pub fn prev_prompt(&mut self, _: &PrevPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_prompt(-1, cx);
    }

    /// ⌘↓: the prompt below the viewport's top row, scrolled to the top; none left means back
    /// to following output.
    pub fn next_prompt(&mut self, _: &NextPrompt, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_prompt(1, cx);
    }

    /// ⌘↑ (`delta` −1) / ⌘↓ (+1), also the phone's armed ⌘ with the bar's ↑ / ↓.
    fn step_prompt(&mut self, delta: i8, cx: &mut Context<Self>) {
        if let Some(conversation) = &self.conversation {
            match conversation.prompt_from_top(delta) {
                Some(ix) => conversation.scroll_to_entry(ix),
                None if delta > 0 => conversation.pin(),
                None => return,
            }
            cx.notify();
            return;
        }
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

    /// ⌘V in a driven composer with a picture on the clipboard: the picture becomes an
    /// attachment of the next prompt instead of text (a text clipboard pastes as text).
    fn composer_paste(
        &mut self,
        _: &gpui_kit::component::input::Paste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.driven || !self.composer_focused(window, cx) {
            return;
        }
        let Some(item) = cx.read_from_clipboard() else { return };
        let pictures: Vec<slopty_proto::agent::Image> = item
            .into_entries()
            .filter_map(|entry| match entry {
                gpui::ClipboardEntry::Image(image) => Some(slopty_proto::agent::Image {
                    media_type: image.format.mime_type().to_owned(),
                    data: image.bytes,
                }),
                gpui::ClipboardEntry::String(_) | gpui::ClipboardEntry::ExternalPaths(_) => None,
            })
            .collect();
        if pictures.is_empty() {
            return;
        }
        cx.stop_propagation();
        for picture in pictures {
            self.attach_image(picture, cx);
        }
    }

    /// Attach a picture to the next prompt. It is made fit off the UI thread
    /// (`attachment::fit`: shrunk and re-encoded when over the model's size or the wire's
    /// cap) and shows as a "preparing" chip meanwhile; a picture the model cannot read, or
    /// one too many, is refused with a notice in the top bar, never sent half.
    pub fn attach_image(&mut self, image: slopty_proto::agent::Image, cx: &mut Context<Self>) {
        use slopty_proto::agent::IMAGES_MAX;
        if self.attachments.len().saturating_add(self.preparing) >= IMAGES_MAX {
            cx.emit(TerminalViewEvent::Notice(format!("At most {IMAGES_MAX} pictures per prompt")));
            return;
        }
        self.preparing = self.preparing.saturating_add(1);
        cx.notify();
        let fitting = cx.background_spawn(async move { super::attachment::fit(image) });
        cx.spawn(async move |this, cx| {
            let fitted = fitting.await;
            let _updated = this.update(cx, |view, cx| {
                view.preparing = view.preparing.saturating_sub(1);
                match fitted {
                    Ok(picture) => view.attachments.push(picture),
                    Err(refused) => {
                        tracing::warn!(session = %view.session, %refused, "picture not attached");
                        cx.emit(TerminalViewEvent::Notice(refused.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Files dropped on the card from the desktop. On a driven card a picture is read off
    /// the UI thread and attached to the next prompt like a pasted one; anything else is
    /// refused by name in the top bar. A shell card takes no files: the paths are this
    /// machine's and the shell runs on the host.
    pub fn drop_paths(&self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        if !self.driven {
            return;
        }
        for path in paths {
            let name = path
                .file_name()
                .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            let Some(media_type) = picture_type(path) else {
                cx.emit(TerminalViewEvent::Notice(format!(
                    "{name}: not a picture (PNG, JPEG, GIF or WebP)"
                )));
                continue;
            };
            let reading = cx.background_spawn({
                let path = path.clone();
                async move { std::fs::read(&path) }
            });
            cx.spawn(async move |this, cx| {
                let read = reading.await;
                let _updated = this.update(cx, |view, cx| match read {
                    Ok(data) => view.attach_image(
                        slopty_proto::agent::Image { media_type: media_type.to_owned(), data },
                        cx,
                    ),
                    Err(e) => cx.emit(TerminalViewEvent::Notice(format!("{name}: {e}"))),
                });
            })
            .detach();
        }
    }

    /// Pictures being made fit right now, not yet attached.
    #[must_use]
    pub const fn preparing(&self) -> usize {
        self.preparing
    }

    /// Attach a host window's (or display's) picture to the next prompt: the host takes it as
    /// the prompt is sent, so nothing is copied here; `title` names it on the chip. Waits for
    /// the conversation when the card has none yet.
    pub fn attach_snapshot(
        &mut self,
        target: slopty_proto::screen::CaptureTarget,
        title: String,
        cx: &mut Context<Self>,
    ) {
        use slopty_proto::agent::IMAGES_MAX;
        let pictures = self.attachments.len().saturating_add(self.preparing);
        if pictures.saturating_add(self.snapshots.len()) >= IMAGES_MAX {
            cx.emit(TerminalViewEvent::Notice(format!("At most {IMAGES_MAX} pictures per prompt")));
            return;
        }
        if self.snapshots.iter().any(|(t, _)| *t == target) {
            return;
        }
        if self.conversation.is_some() {
            self.snapshots.push((target, title));
            self.focus_composer = true;
        } else {
            self.pending_snapshot = Some((target, title));
        }
        cx.notify();
    }

    /// Drop the `i`th window attachment (its chip was tapped).
    pub fn remove_snapshot(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.snapshots.len() {
            self.snapshots.remove(i);
            cx.notify();
        }
    }

    /// The windows whose pictures go with the next prompt, with their titles.
    #[must_use]
    pub fn snapshots(&self) -> &[(slopty_proto::screen::CaptureTarget, String)] {
        &self.snapshots
    }

    /// Drop the `i`th attachment (its chip was tapped).
    pub fn remove_attachment(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.attachments.len() {
            self.attachments.remove(i);
            cx.notify();
        }
    }

    /// The pictures waiting to go with the next prompt.
    #[must_use]
    pub fn attachments(&self) -> &[slopty_proto::agent::Image] {
        &self.attachments
    }

    /// ⌘V, or the phone key bar's "paste": the clipboard into the session (the host brackets
    /// it when the program asked). On a driven card there is no session to paste into: a
    /// picture becomes an attachment of the next prompt and text goes into the composer.
    pub fn paste_clipboard(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        if self.driven {
            let mut text = None;
            for entry in item.into_entries() {
                match entry {
                    gpui::ClipboardEntry::Image(image) => self.attach_image(
                        slopty_proto::agent::Image {
                            media_type: image.format.mime_type().to_owned(),
                            data: image.bytes,
                        },
                        cx,
                    ),
                    gpui::ClipboardEntry::String(string) => text = Some(string.text().to_owned()),
                    gpui::ClipboardEntry::ExternalPaths(_) => {}
                }
            }
            if let (Some(text), Some(conversation)) = (text, &self.conversation) {
                conversation.insert_composer_text(&text, window, cx);
            }
            cx.notify();
            return;
        }
        let Some(text) = item.text() else { return };
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
        // An armed ⌘ with the bar's ↑ / ↓ is ⌘↑ / ⌘↓: between prompts, not to the program.
        if self.sticky_command && matches!(keystroke.key.as_str(), "up" | "down") {
            self.sticky_command = false;
            self.step_prompt(if keystroke.key == "up" { -1 } else { 1 }, cx);
            return;
        }
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

    /// A key of the phone's bar. In a conversation the bar's ↑ / ↓ are the composer's, as a
    /// hardware keyboard's would be — the completion list's while it is up, else a prompt
    /// sent back ([`Conversation::recall`]) — since the program behind the card is not on
    /// show for a raw arrow to mean anything there; everything else is [`Self::press`].
    pub fn bar_key(&mut self, keystroke: Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let delta = match keystroke.key.as_str() {
            "up" => -1,
            "down" => 1,
            _ => 0,
        };
        if delta != 0 && !self.sticky_command && self.conversation.is_some() {
            let completions = self.completions(cx);
            if let Some(c) = self.conversation.as_mut() {
                if completions.is_empty() {
                    let _recalled = c.recall(delta, window, cx);
                } else {
                    c.step_completion(i32::from(delta), completions.len());
                }
            }
            cx.notify();
            return;
        }
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
                Effect::CommandStarted(_) => self.command_started = Some(Instant::now()),
                Effect::CommandFinished { prompt, command, exit } => {
                    let elapsed =
                        self.command_started.take().map_or(Duration::ZERO, |t| t.elapsed());
                    self.set_took(prompt, elapsed);
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
        // Esc closes the block menu and goes no further.
        if self.block_menu.is_some() && event.keystroke.key == "escape" {
            self.block_menu = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
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
            if self.driven {
                // No grid behind the chat: Esc and ⌃C interrupt the agent, nothing else leaves
                // the composer. (The completion list's keys were taken in the capture phase,
                // see `completion_key`.)
                if k.key == "escape" || (k.modifiers.control && k.key == "c") {
                    self.interrupt_agent();
                    cx.stop_propagation();
                } else if k.key == "enter" && !k.modifiers.shift {
                    cx.stop_propagation();
                }
                return;
            }
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
        // ⌘-wheel is the canvas's zoom, never the grid's.
        if event.modifiers.platform {
            self.wheel_remainder = 0.0;
            return;
        }
        // A program that asked for the mouse gets the wheel (⇧ keeps it for scrolling, as
        // in every terminal); so does anything on the alternate screen, which has no
        // history here to scroll — the host turns it into cursor keys (alternate scroll).
        let modes = self.state.modes();
        let to_program = (modes.contains(TermModes::MOUSE_TRACKING) && !event.modifiers.shift)
            || modes.contains(TermModes::ALT_SCREEN);
        // The grid takes the wheel while it can use it — a program wants it, or there is
        // history that way — and otherwise lets it through, so the canvas pans under a grid
        // at the end of its history rather than swallowing the gesture.
        let usable = to_program
            || (lines > 0.0 && self.state.view_offset() < self.state.history_len())
            || (lines < 0.0 && self.state.view_offset() > 0);
        if !usable {
            self.wheel_remainder = 0.0;
            return;
        }
        cx.stop_propagation();
        // A trackpad moves in fractions of a line: they add up to whole ones, and a new
        // gesture starts the count over.
        if event.touch_phase == TouchPhase::Started {
            self.wheel_remainder = 0.0;
        }
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
        // The conversation's controls (composer, folds, buttons) take their own clicks.
        if self.conversation.is_some() {
            return;
        }
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
                self.open_path(&span, armed, cx);
            } else if armed && let Some(block) = self.state.command_block(index) {
                // The phone has no right button: the armed tap on a bare block row is its
                // menu.
                self.block_menu = Some(BlockMenu { block, at: event.position });
                cx.notify();
            }
            return;
        }
        // Left button selects unless the program asked for the mouse (⇧ overrides, as in
        // every terminal); everything else is reported to the program.
        let program_wants_mouse = self.state.modes().contains(TermModes::MOUSE_TRACKING);
        // Right button on a command block (shell integration marks them) opens its menu.
        if event.button == MouseButton::Right
            && (!program_wants_mouse || event.modifiers.shift)
            && let Some(block) = self.state.command_block(self.state.index_at_row(row))
        {
            self.block_menu = Some(BlockMenu { block, at: event.position });
            cx.notify();
            return;
        }
        if event.button == MouseButton::Left && (!program_wants_mouse || event.modifiers.shift) {
            let index = self.state.index_at_row(row);
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

    /// The pointer moved over the grid: the hover for the ⌘ underline, and the scrollbar
    /// shows itself while the pointer is over the card (a drag is followed by
    /// [`Self::drag_move`], which the element registers on the window so the drag can leave
    /// the card).
    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.conversation.is_some() {
            return;
        }
        let shown = self.scrollbar_shown();
        let hover = self.metrics.and_then(|m| m.cell_at(event.position));
        self.set_pointer(hover, event.modifiers.platform, cx);
        if shown != self.scrollbar_shown() {
            cx.notify();
        }
    }

    /// A drag with the left button, wherever the pointer is: the thumb held scrolls the
    /// viewport with it; a selection follows the pointer, and past the grid's top or bottom
    /// it keeps scrolling (`AUTOSCROLL_TICK`) at a pace set by how far past the pointer is.
    pub(super) fn drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
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
        if self.thumb_drag.take().is_some() {
            cx.notify();
            return;
        }
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
        floating: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let wash = hsla_alpha(s.text, alpha::HOVER);
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
            .when(floating, |bar| {
                bar.absolute()
                    .top(px(spacing.sm))
                    // A phone-wide terminal can be wider than the screen; its left edge is
                    // the part that is on screen (the "take" pill sits there for the same
                    // reason).
                    .when(cfg!(target_os = "ios"), |bar| bar.left(px(spacing.sm)))
                    .when(!cfg!(target_os = "ios"), |bar| bar.right(px(spacing.sm)))
            })
            .when(!floating, |bar| bar.flex_none().mx(px(spacing.sm)).mt(px(spacing.xs)))
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
        let opened = std::mem::take(&mut self.open_conversation) && self.conversation.is_none();
        if opened {
            // A driven view opens its chat on its first frame (it needs the window) and asks
            // for the conversation so far, which may have arrived before there was a chat.
            self.conversation = Some(Conversation::new(window, cx));
            self.focus_composer = true;
            self.say(ClientMsg::Transcript(TranscriptFollow {
                session: self.session,
                follow: true,
            }));
            cx.notify();
        } else if let Some(conversation) = &self.conversation
            && let Some(text) = self.pending_compose.take()
        {
            // The frame after the composer first drew: an insert into an input that has never
            // been laid out is lost.
            conversation.append_composer_text(&text, window, cx);
            self.focus_composer = true;
        }
        if self.conversation.is_some()
            && let Some(snapshot) = self.pending_snapshot.take()
        {
            self.snapshots.push(snapshot);
            self.focus_composer = true;
            cx.notify();
        }
        if self.driven && focused && self.conversation.is_some() {
            // The canvas gives the view the keyboard; in a driven view that is the composer.
            self.focus_composer = true;
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
        let info = self.driven.then_some(&self.info);
        let working = self.driven && matches!(self.agent_status(), Some(AgentStatus::Working));
        let search_focused = self
            .search
            .as_ref()
            .is_some_and(|s| s.input.read(cx).focus_handle(cx).is_focused(window));
        // Over the grid the bar floats in a corner; in a conversation it is a row under the
        // header, so it covers no chip and no line of the chat.
        let floating = self.conversation.is_none();
        let mut search =
            self.search.as_ref().map(|s| self.render_search(s, search_focused, floating, cx));
        let conversation = self.conversation.as_ref().map(|c| {
            c.render(
                info,
                attention.as_ref(),
                &self.partial,
                &self.attachments,
                &self.snapshots,
                self.preparing,
                composer_focused,
                working,
                self.files.as_ref(),
                self.can_run_in_shell,
                search.take(),
                &self.theme,
                cx,
            )
        });
        let header = self
            .conversation
            .is_none()
            .then(|| self.block_header())
            .flatten()
            .and_then(|block| self.render_block_header(&block, cx));
        div()
            .id("terminal")
            .debug_selector(|| "terminal".to_owned())
            .key_context("Terminal")
            .track_focus(&self.focus)
            .role(gpui::accesskit::Role::Group)
            .relative()
            .size_full()
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _window, cx| {
                this.drop_paths(paths.paths(), cx);
            }))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_prev))
            .on_action(cx.listener(Self::prev_prompt))
            .on_action(cx.listener(Self::next_prompt))
            .on_action(cx.listener(Self::copy_last_output))
            .on_action(cx.listener(Self::copy_conversation))
            .on_action(cx.listener(Self::rerun_last))
            .on_action(cx.listener(Self::note_last_block))
            .on_action(cx.listener(Self::clear_screen))
            .on_action(cx.listener(Self::toggle_conversation_action))
            .on_action(cx.listener(|this, _: &CompleteSlash, window, cx| {
                if !this.completion_nav("tab", window, cx) {
                    // A grid, or nothing to complete: the key goes on to whoever is next.
                    cx.propagate();
                }
            }))
            .capture_key_down(cx.listener(Self::completion_key))
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::MoveDown, window, cx| {
                    this.composer_action("down", window, cx);
                },
            ))
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::MoveUp, window, cx| {
                    this.composer_action("up", window, cx);
                },
            ))
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::Escape, window, cx| {
                    this.composer_action("escape", window, cx);
                },
            ))
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::IndentInline, window, cx| {
                    this.composer_action("tab", window, cx);
                },
            ))
            // ⌘F with the caret in the composer: the input's own search action would take
            // it; the card's find bar is what a reader means.
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::Search, window, cx| {
                    if this.conversation.is_some() {
                        this.find(&Find, window, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            // ⌘↑ / ⌘↓ with the caret in the composer: the input's own start/end actions
            // would take them; between prompts is what a reader means.
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::MoveToStart, window, cx| {
                    if this.conversation.is_some() {
                        this.prev_prompt(&PrevPrompt, window, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::MoveToEnd, window, cx| {
                    if this.conversation.is_some() {
                        this.next_prompt(&NextPrompt, window, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(cx.listener(Self::composer_paste))
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
            .children(header)
            .children(search)
            .children(self.block_menu.as_ref().map(|m| self.render_block_menu(m, cx)))
    }
}

/// The command-block menu a right click opened.
struct BlockMenu {
    /// The block under the click.
    block: CommandBlock,
    /// Where the click landed (window coordinates), the menu's anchor.
    at: gpui::Point<Pixels>,
}

/// What the block menu offers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockMenuItem {
    /// The typed command to the clipboard.
    CopyCommand,
    /// The command's output to the clipboard.
    CopyOutput,
    /// Type the command again and press ↩.
    Rerun,
    /// The block, as a fence, into the agent's composer.
    Ask,
    /// The block as a note card beside the shell: the command runnable, the output under it.
    Note,
    /// Select the whole block, prompt to last output row.
    SelectBlock,
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

/// A command block as the agent should read it: the command on a `$` line and its output,
/// fenced, with a blank line after for the question.
#[must_use]
pub fn block_markdown(block: &CommandBlock) -> String {
    let mut text = String::from("```\n");
    if let Some(command) = &block.command {
        text.push_str("$ ");
        text.push_str(command);
        text.push('\n');
    }
    if !block.output.is_empty() {
        text.push_str(&block.output);
        text.push('\n');
    }
    text.push_str("```\n\n");
    text
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
            Self::Ask => "ask",
            Self::Note => "note",
            Self::SelectBlock => "select-block",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::CopyCommand => "Copy command",
            Self::CopyOutput => "Copy output",
            Self::Rerun => "Rerun",
            Self::Ask => "Ask the agent",
            Self::Note => "Save as note",
            Self::SelectBlock => "Select block",
        }
    }
}

/// The media type of a picture file the model reads, from its extension; `None` for any
/// other file.
#[must_use]
pub fn picture_type(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Pixels, TestAppContext, VisualTestContext, px, size};
    use slopty_grid::{Line, RowUpdate, SemanticMark, Style, TermModes};
    use slopty_proto::agent::{Clipped, SlashCommand, ToolDetail, TranscriptBody, TranscriptEntry};
    use slopty_proto::terminal::{Frame, TermRequest};

    use super::*;
    use crate::terminal::conversation::attachment_label;

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

    /// A real picture of `width`×`height` encoded as `format`, for pasting.
    fn encoded(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let buffer = image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgb([u8::try_from(x % 256).unwrap_or(0), u8::try_from(y % 256).unwrap_or(0), 90])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(buffer).write_to(&mut out, format).expect("encodes");
        out.into_inner()
    }

    fn user(text: &str) -> TranscriptEntry {
        TranscriptEntry {
            at: None,
            body: TranscriptBody::User { text: text.to_owned(), images: 0 },
        }
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
                call: format!("toolu_{command}"),
                name: "Bash".to_owned(),
                summary: command.to_owned(),
                detail: ToolDetail::Command {
                    command: Clipped::whole(command.to_owned()),
                    description: None,
                },
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
    /// any click close it; a row before the first prompt offers nothing.
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
            "Ask the agent",
            "Save as note",
            "Select block",
        ] {
            assert!(tree.iter().any(|n| n.is("MenuItem", Some(label))), "{label}: {tree:#?}");
        }
        let block = view.read_with(cx, |v, _| v.block_menu.as_ref().map(|m| m.block.clone()));
        let block = block.expect("the menu holds its block");
        assert_eq!(block_markdown(&block), "```\n$ seq 2\n1\n2\n```\n\n");
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
        let tapped = view.read_with(cx, |v, _| v.block_menu.as_ref().map(|m| m.block.clone()));
        assert_eq!(
            tapped.map(|b| block_markdown(&b)).as_deref(),
            Some("```\n$ seq 2\n1\n2\n```\n\n")
        );
        assert!(drain_input(&mut rx).is_empty(), "the tap is not reported");
        pick(cx, "copy-command");
        assert_eq!(clipboard(cx).as_deref(), Some("seq 2"));
        // Disarmed, a tap on the same row is a plain click: no menu.
        cx.simulate_click(at, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("block-menu").is_none());
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

    /// A trackpad's fractions of a line add up to whole lines scrolled; a program tracking
    /// the mouse gets the wheel as rows (⇧ keeps it local); so does the alternate screen.
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
            entries: vec![
                user("fix it"),
                assistant("On it.\n\n```sh\ncargo test\n```\n\nthen look."),
            ],
        };
        view.update(cx, |v, cx| v.transcript_update(snapshot, cx));
        let more = TranscriptUpdate { session, reset: false, entries: vec![tool("cargo test")] };
        view.update(cx, |v, cx| v.transcript_update(more, cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.conversation().map(|c| c.entries().len())), Some(3));
        let bounds = cx.debug_bounds("conversation").expect("drawn");
        assert!(bounds.size.width > px(0.0) && bounds.size.height > px(0.0));

        // The fenced block in the answer is its own element with a copy button: a click puts
        // the code alone on the clipboard.
        let copy = cx.debug_bounds("conversation-code-copy-1-1").expect("the block's copy button");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("Copy code"))), "{tree:#?}");
        cx.simulate_click(copy.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("cargo test"));
        // The answer's own copy button takes the whole answer; the palette's action the whole
        // conversation as Markdown.
        let copy = cx.debug_bounds("conversation-copy-1").expect("the answer's copy button");
        cx.simulate_click(copy.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("On it.\n\n```sh\ncargo test\n```\n\nthen look."));
        // Its "note" button asks the canvas for a card with the answer, as a block's does.
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
        let note = cx.debug_bounds("conversation-note-1").expect("the answer's note button");
        cx.simulate_click(note.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            notes.borrow().as_slice(),
            ["On it.\n\n```sh\ncargo test\n```\n\nthen look."],
            "the answer's Markdown, as copied"
        );
        cx.update(|window, cx| window.dispatch_action(Box::new(CopyConversation), cx));
        cx.run_until_parked();
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(
                "**You**\n\nfix it\n\n**Claude**\n\nOn it.\n\n```sh\ncargo test\n```\n\nthen look.\n\n> **Bash** cargo test\n"
            )
        );
        assert_eq!(
            conversation::segments("a\n```sh\nx\n```\nb"),
            [
                conversation::Segment::Prose("a".to_owned()),
                conversation::Segment::Code { lang: "sh".to_owned(), body: "x".to_owned() },
                conversation::Segment::Prose("b".to_owned()),
            ]
        );
        assert_eq!(
            conversation::segments("```\nopen"),
            [conversation::Segment::Code { lang: String::new(), body: "open".to_owned() }],
            "an unclosed fence runs to the end"
        );
        assert_eq!(
            conversation::segments("just prose"),
            [conversation::Segment::Prose("just prose".to_owned())]
        );

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
            Some(Attention::Permission {
                tool: "Bash".to_owned(),
                answered: Some(true),
                detail: None,
                always: None,
            })
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

    /// An edit shows as its diff unasked — added lines tinted in the success tone, removed
    /// in the error tone, the counts in the header — folded past its preview until a click;
    /// a todo list shows whole, with what is done struck through; a screen reader hears the
    /// counts.
    #[gpui::test]
    fn an_edit_shows_its_diff_and_a_todo_list_its_checklist(cx: &mut TestAppContext) {
        use slopty_proto::agent::{DiffKind, DiffLine, Todo, TodoStatus};
        let (view, _rx, cx) = terminal(cx);
        // Tall enough for the whole open diff: the list is bottom-aligned and would scroll
        // the top rows out of the scene otherwise.
        cx.simulate_resize(size(px(400.0), px(900.0)));
        cx.simulate_keystrokes("cmd-shift-l");
        let session = view.read_with(cx, |v, _| v.session);
        let line = |kind, text: &str| DiffLine { kind, text: text.to_owned() };
        let mut lines = vec![line(DiffKind::Context, "fn a() {")];
        lines.extend((0..10).map(|i| line(DiffKind::Removed, &format!("    old {i}"))));
        lines.extend((0..10).map(|i| line(DiffKind::Added, &format!("    new {i}"))));
        lines.push(line(DiffKind::Context, "}"));
        let edit = TranscriptEntry {
            at: None,
            body: TranscriptBody::ToolUse {
                call: "toolu_e".to_owned(),
                name: "Edit".to_owned(),
                summary: "src/a.rs".to_owned(),
                detail: ToolDetail::Diff {
                    path: "src/a.rs".to_owned(),
                    line: None,
                    lines,
                    more_lines: 0,
                    replace_all: false,
                },
            },
        };
        let todos = TranscriptEntry {
            at: None,
            body: TranscriptBody::ToolUse {
                call: "toolu_t".to_owned(),
                name: "TodoWrite".to_owned(),
                summary: String::new(),
                detail: ToolDetail::Todos {
                    items: vec![
                        Todo { text: "read".to_owned(), status: TodoStatus::Completed },
                        Todo { text: "write".to_owned(), status: TodoStatus::InProgress },
                        Todo { text: "test".to_owned(), status: TodoStatus::Pending },
                    ],
                },
            },
        };
        let update = TranscriptUpdate { session, reset: true, entries: vec![edit, todos] };
        view.update(cx, |v, cx| v.transcript_update(update, cx));
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();

        let theme = view.read_with(cx, |v, _| v.theme().clone());
        let s = &theme.surfaces;
        let tinted = |cx: &mut VisualTestContext, tone| {
            let want = hsla_alpha(tone, alpha::TINT);
            let quads = cx.update(|window, _| window.painted_quads());
            quads.iter().filter(|q| q.background.as_solid().is_some_and(|c| c == want)).count()
        };
        // Folded: the preview's lines, ten removed and two added.
        assert_eq!(tinted(cx, s.error), 10, "removed lines in the error tint");
        assert_eq!(tinted(cx, s.success), 1, "added lines in the success tint, folded");
        let folded = cx.debug_bounds("conversation-entry-0").expect("the edit");
        let header = point(folded.center().x, folded.top() + px(6.0));
        cx.simulate_click(header, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(tinted(cx, s.success), 10, "every added line once open");
        let open = cx.debug_bounds("conversation-entry-0").expect("the edit");
        assert!(open.size.height > folded.size.height);
        assert!(cx.debug_bounds("conversation-entry-1").is_some(), "the todo list is drawn");

        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let labels: Vec<String> =
            tree.iter().filter(|n| n.role == "ListItem").filter_map(|n| n.label.clone()).collect();
        assert_eq!(
            labels,
            ["Tool Edit: src/a.rs, 10 added, 10 removed", "Tool TodoWrite: 1 of 3 done"],
            "{tree:#?}"
        );
    }

    /// A subagent's progress shows under the call that spawned it — the kind, the tool
    /// count, the time, the last tool, then "done" — and a screen reader hears the same;
    /// the subagent's own records never become entries (that is the host's rule, tested
    /// in `slopty_agent`), so the card stays the agent's conversation.
    #[gpui::test]
    fn a_subagent_progresses_under_its_call(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        cx.simulate_keystrokes("cmd-shift-l");
        let session = view.read_with(cx, |v, _| v.session);
        let spawn = TranscriptEntry {
            at: None,
            body: TranscriptBody::ToolUse {
                call: "toolu_p".to_owned(),
                name: "Agent".to_owned(),
                summary: "List files".to_owned(),
                detail: ToolDetail::Agent {
                    description: "List files".to_owned(),
                    kind: Some("Explore".to_owned()),
                    prompt: Clipped::whole("ls".to_owned()),
                },
            },
        };
        let update = TranscriptUpdate { session, reset: true, entries: vec![spawn] };
        view.update(cx, |v, cx| v.transcript_update(update, cx));
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let label = |cx: &mut VisualTestContext| {
            let tree = cx.update(|window, _cx| crate::a11y::tree(window));
            tree.iter()
                .find(|n| n.role == "ListItem")
                .and_then(|n| n.label.clone())
                .unwrap_or_default()
        };
        assert_eq!(label(cx), "Tool Agent: List files");
        let task = |tool_uses, duration_ms, done| AgentTask {
            call: "toolu_p".to_owned(),
            description: "Running List files".to_owned(),
            kind: Some("Explore".to_owned()),
            tool_uses,
            duration_ms,
            last_tool: Some("Bash".to_owned()),
            done,
        };
        view.update(cx, |v, cx| v.agent_task(task(1, 2_525, false), cx));
        cx.run_until_parked();
        assert_eq!(label(cx), "Tool Agent: List files, Explore running, 1 tool use, 3 s, Bash");
        assert!(cx.debug_bounds("tool-task").is_some(), "the progress line is drawn");
        view.update(cx, |v, cx| v.agent_task(task(3, 12_000, true), cx));
        cx.run_until_parked();
        assert_eq!(label(cx), "Tool Agent: List files, Explore done, 3 tool uses, 12 s, Bash");
        // A reset (a new conversation) forgets the tasks.
        let again = TranscriptUpdate { session, reset: true, entries: vec![user("ok")] };
        view.update(cx, |v, cx| v.transcript_update(again, cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.conversation().is_some_and(|c| c.tasks().is_empty())));
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

    /// A tool result's lines that name a file — a grep hit, a compiler's location — are
    /// buttons that view the file on the canvas at that line, relative to the agent's cwd.
    #[gpui::test]
    fn a_results_path_lines_view_the_file(cx: &mut TestAppContext) {
        let (view, _rx, cx) = terminal(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
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
        cx.simulate_keystrokes("cmd-shift-l");
        let session = view.read_with(cx, |v, _| v.session);
        let result = TranscriptEntry {
            at: None,
            body: TranscriptBody::ToolResult {
                tool: Some("Grep".to_owned()),
                output: Clipped::whole(
                    "src/a.rs:12:fn x() {\nno path here\n  --> src/b.rs:3:5".to_owned(),
                ),
                is_error: false,
            },
        };
        view.update(cx, |v, cx| {
            v.agent_info(AgentInfo { cwd: Some("/w".to_owned()), ..AgentInfo::default() }, cx);
            v.transcript_update(
                TranscriptUpdate { session, reset: true, entries: vec![tool("grep"), result] },
                cx,
            );
        });
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let buttons: Vec<&str> = tree
            .iter()
            .filter(|n| {
                n.role == "Button" && n.label.as_deref().is_some_and(|l| l.starts_with("View "))
            })
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(
            buttons,
            ["View src/a.rs:12 on the canvas", "View src/b.rs:3 on the canvas"],
            "{tree:#?}"
        );
        assert!(cx.debug_bounds("result-path-1-1").is_none(), "a line without a path is text");
        let hit = cx.debug_bounds("result-path-1-2").expect("the compiler's location");
        cx.simulate_click(hit.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(viewed.borrow().as_slice(), [("/w/src/b.rs".to_owned(), Some(3))]);
        assert!(
            view.read_with(cx, |v, _| v.conversation().is_some_and(|c| !c.is_open(1))),
            "the click did not toggle the fold"
        );
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

    /// Every message the host received that speaks to or about the agent, oldest first, as
    /// one word each.
    fn drain_words(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(match msg {
                ClientMsg::Transcript(TranscriptFollow { follow, .. }) => {
                    format!("follow:{follow}")
                }
                ClientMsg::AgentSay { text, .. } => format!("say:{text}"),
                ClientMsg::AgentAnswer(answer) => {
                    let filed: Vec<String> = answer
                        .answers
                        .iter()
                        .map(|a| format!(":{}={}", a.question, a.answer))
                        .collect();
                    let tail = if answer.always { ":always" } else { "" };
                    format!("answer:{}:{}{}{tail}", answer.request, answer.allowed, filed.concat())
                }
                ClientMsg::AgentInterrupt { .. } => "interrupt".to_owned(),
                ClientMsg::AgentSet(AgentSet { model, permission_mode, .. }) => format!(
                    "set:{}{}",
                    model.map(|m| format!("model={m}")).unwrap_or_default(),
                    permission_mode.map(|m| format!("mode={m}")).unwrap_or_default()
                ),
                ClientMsg::Term { req: TermRequest::Key(_), .. } => "key".to_owned(),
                ClientMsg::Term { req: TermRequest::Paste(_), .. } => "paste".to_owned(),
                ClientMsg::Term { req: TermRequest::Clear, .. } => "clear".to_owned(),
                ClientMsg::ListFiles { query, .. } => format!("files:{query}"),
                // Resizes and the like: not what these tests are about.
                _ => continue,
            });
        }
        out
    }

    /// A driven view (the host speaks Claude Code's stream-json protocol) opens its
    /// conversation on its first frame with the keyboard in the composer and asks the host for
    /// the conversation so far; ↩ speaks to the agent rather than pasting into a grid; the
    /// agent's streamed text shows under the list until it becomes an entry; a permission
    /// request shows what the tool would do and is answered by request id, never by a key;
    /// Esc interrupts; ⌘⇧L cannot hide the conversation.
    #[gpui::test]
    fn a_driven_view_speaks_to_the_agent(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        let answers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&answers);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::AgentAnswered { request, allowed, always, .. } = event {
                    let tail = if *always { ":always" } else { "" };
                    seen.borrow_mut().push(format!("{request}:{allowed}{tail}"));
                }
            })
            .detach();
        });
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation").is_some(), "the conversation is the view");
        assert!(composer_focused(&view, cx), "with the caret in the composer");
        assert_eq!(drain_words(&mut rx), ["follow:true"]);

        cx.simulate_keystrokes("h i enter");
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["say:hi"], "↩ speaks to the agent");
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some(""));
        cx.simulate_keystrokes("enter");
        assert!(drain_words(&mut rx).is_empty(), "an empty ↩ says nothing");

        view.update(cx, |v, cx| v.agent_partial("Hello, so far".to_owned(), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation-partial").is_some(), "the partial shows");
        view.update(cx, |v, cx| v.agent_partial(String::new(), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation-partial").is_none());

        let request = PermissionRequest {
            id: "req_1".to_owned(),
            tool_use: "toolu_1".to_owned(),
            tool: "Write".to_owned(),
            summary: "note.txt".to_owned(),
            detail: ToolDetail::Json { input: Clipped::whole("{}".to_owned()) },
            always: Some("accept edits for this session".to_owned()),
        };
        view.update(cx, |v, cx| {
            v.agent_permission(request, cx);
            let blocked =
                AgentStatus::Blocked(BlockReason::Permission { tool: "Write".to_owned() });
            v.set_agent_status(Some(blocked), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.attention()),
            Some(Attention::Permission {
                tool: "Write".to_owned(),
                answered: None,
                detail: Some("note.txt".to_owned()),
                always: Some("accept edits for this session".to_owned()),
            })
        );
        // Always sits between Allow and Deny and says what it would do; a tap on it allows
        // with the agent's suggestion taken.
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let at = |role: &str, label: &str| {
            tree.iter()
                .position(|n| n.is(role, Some(label)))
                .unwrap_or_else(|| panic!("{role} {label:?} in {tree:#?}"))
        };
        assert!(at("Button", "Allow") < at("Button", "Always: accept edits for this session"));
        assert!(at("Button", "Always: accept edits for this session") < at("Button", "Deny"));
        let always = cx.debug_bounds("conversation-always").expect("Always is drawn");
        cx.simulate_click(always.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*answers.borrow(), ["req_1:true:always"], "answered by request id, for good");
        assert!(drain_words(&mut rx).is_empty(), "no key was typed");
        assert!(cx.debug_bounds("conversation-allow").is_none(), "one tap, one answer");
        assert!(cx.debug_bounds("conversation-always").is_none());

        view.update(cx, |v, cx| v.set_agent_status(Some(AgentStatus::Working), cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.permission().is_none()), "the request is spent");
        cx.simulate_keystrokes("escape");
        assert_eq!(drain_words(&mut rx), ["interrupt"], "Esc interrupts the turn");
        cx.simulate_keystrokes("ctrl-c");
        assert_eq!(drain_words(&mut rx), ["interrupt"], "so does ⌃C");
        assert!(composer_focused(&view, cx), "and the caret stays");
        // While the agent works the button next to the field is Stop, one tap to interrupt;
        // once it is idle again it is Send.
        assert!(cx.debug_bounds("composer-send").is_none(), "no Send while working");
        let stop = cx.debug_bounds("composer-stop").expect("Stop is drawn while working");
        cx.simulate_click(stop.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["interrupt"], "Stop interrupts the turn");
        view.update(cx, |v, cx| v.set_agent_status(Some(AgentStatus::Idle), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("composer-stop").is_none(), "no Stop when idle");
        assert!(cx.debug_bounds("composer-send").is_some(), "Send is back");

        cx.simulate_keystrokes("cmd-shift-l");
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation").is_some(), "the conversation cannot be hidden");
        assert!(drain_words(&mut rx).is_empty());
    }

    /// ⌘F in a driven card searches the conversation itself, no host round trip: the bar sits
    /// under the header, the hits are the entries holding the needle (newest first on
    /// show), ⌘G / ↩ / ⇧↩ step and wrap, `.*` makes the needle a regex (a bad one says so),
    /// new entries keep the hits true, and Esc closes the bar with the caret back in the
    /// composer.
    /// ↩ while the agent works still sends (Claude Code queues the prompt for its next turn),
    /// and the conversation shows it as queued until its entry arrives or the turn ends.
    #[gpui::test]
    fn a_prompt_sent_while_the_agent_works_waits_as_queued(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        drain_words(&mut rx);
        let session = view.read_with(cx, |v, _| v.session);
        let update = |reset: bool, entries: Vec<TranscriptEntry>| TranscriptUpdate {
            session,
            reset,
            entries,
        };
        view.update(cx, |v, cx| {
            v.transcript_update(update(true, vec![user("first"), assistant("one")]), cx);
            v.set_agent_status(Some(AgentStatus::Working), cx);
        });
        cx.run_until_parked();
        let queued = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.conversation().map(|c| c.queued().to_vec()))
        };
        cx.simulate_keystrokes("l a t e r enter");
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["say:later"], "sent, not held back");
        assert_eq!(queued(cx).as_deref(), Some(&["later".to_owned()][..]));
        assert!(cx.debug_bounds("conversation-queued-0").is_some(), "drawn as queued");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Status", Some("Queued: later"))), "{tree:#?}");
        view.update(cx, |v, cx| {
            v.set_agent_status(Some(AgentStatus::Tool { tool: "Bash".to_owned() }), cx);
        });
        cx.simulate_keystrokes("m o r e enter");
        cx.run_until_parked();
        drain_words(&mut rx);
        assert_eq!(queued(cx).map(|q| q.len()), Some(2), "a tool is still the turn");
        // Its entry arrives: that one is off the queue, the other still waits.
        view.update(cx, |v, cx| {
            v.transcript_update(update(false, vec![user("later"), assistant("two")]), cx);
        });
        cx.run_until_parked();
        assert_eq!(queued(cx).as_deref(), Some(&["more".to_owned()][..]));
        assert!(cx.debug_bounds("conversation-queued-1").is_none());
        // Blocked on the human mid-turn keeps the queue; the turn's end clears it.
        view.update(cx, |v, cx| {
            v.set_agent_status(Some(AgentStatus::Blocked(BlockReason::Question)), cx);
        });
        assert_eq!(queued(cx).map(|q| q.len()), Some(1));
        view.update(cx, |v, cx| v.set_agent_status(Some(AgentStatus::Done), cx));
        cx.run_until_parked();
        assert_eq!(queued(cx).map(|q| q.len()), Some(0));
        assert!(cx.debug_bounds("conversation-queued-0").is_none());
        // Sent while idle: nothing is queued.
        view.update(cx, |v, cx| v.set_agent_status(Some(AgentStatus::Idle), cx));
        cx.simulate_keystrokes("n o w enter");
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["say:now"]);
        assert_eq!(queued(cx).map(|q| q.len()), Some(0));
        // A reset (a resumed card) drops whatever was queued.
        view.update(cx, |v, cx| v.set_agent_status(Some(AgentStatus::Working), cx));
        cx.simulate_keystrokes("x enter");
        cx.run_until_parked();
        assert_eq!(queued(cx).map(|q| q.len()), Some(1));
        view.update(cx, |v, cx| v.transcript_update(update(true, vec![user("first")]), cx));
        assert_eq!(queued(cx).map(|q| q.len()), Some(0));
    }

    #[gpui::test]
    fn the_composer_recalls_sent_prompts_on_up_and_down(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        drain_words(&mut rx);
        let session = view.read_with(cx, |v, _| v.session);
        view.update(cx, |v, cx| {
            let entries = vec![
                user("first"),
                assistant("one"),
                user("second"),
                user("second"),
                assistant("two"),
                user("third"),
                assistant("three"),
            ];
            v.transcript_update(TranscriptUpdate { session, reset: true, entries }, cx);
        });
        cx.run_until_parked();
        let text = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)))
        };
        let up_down = |cx: &mut VisualTestContext, keys: &str| {
            cx.simulate_keystrokes(keys);
            cx.run_until_parked();
        };
        assert!(composer_focused(&view, cx));
        // ↑ on the empty composer: the newest prompt; ↑ again the older ones, the oldest
        // stays; a prompt sent twice in a row is one step.
        up_down(cx, "up");
        assert_eq!(text(cx).as_deref(), Some("third"));
        up_down(cx, "up");
        assert_eq!(text(cx).as_deref(), Some("second"));
        up_down(cx, "up up");
        assert_eq!(text(cx).as_deref(), Some("first"));
        // ↓ back down; past the newest the draft (empty) is back.
        up_down(cx, "down down");
        assert_eq!(text(cx).as_deref(), Some("third"));
        up_down(cx, "down");
        assert_eq!(text(cx).as_deref(), Some(""));
        // A draft is kept aside while recalling and put back past the newest.
        up_down(cx, "d r a f t up");
        assert_eq!(text(cx).as_deref(), Some("draft"), "↑ in the human's own text moves the caret");
        up_down(cx, "cmd-a backspace up down");
        assert_eq!(text(cx).as_deref(), Some(""));
        up_down(cx, "d up");
        assert_eq!(text(cx).as_deref(), Some("d"));
        up_down(cx, "backspace up");
        assert_eq!(text(cx).as_deref(), Some("third"));
        // Typing into a recalled prompt makes it the human's own: ↑ moves the caret.
        up_down(cx, "x up");
        assert_eq!(text(cx).as_deref(), Some("thirdx"), "the caret was at the end");
        // Sending forgets the recall: ↑ starts from the newest again.
        up_down(cx, "enter");
        assert_eq!(text(cx).as_deref(), Some(""));
        drain_words(&mut rx);
        up_down(cx, "up");
        assert_eq!(text(cx).as_deref(), Some("third"), "the transcript has not grown yet");

        // The phone's bar ↑ / ↓ are the composer's too, not raw keys to the program.
        let bar = |cx: &mut VisualTestContext, key: &str| {
            let stroke = Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: key.into(),
                key_char: None,
            };
            view.update_in(cx, |v, window, cx| v.bar_key(stroke, window, cx));
            cx.run_until_parked();
        };
        bar(cx, "up");
        assert_eq!(text(cx).as_deref(), Some("second"));
        bar(cx, "down");
        assert_eq!(text(cx).as_deref(), Some("third"));
        assert!(rx.try_recv().is_err(), "nothing went to the program");
        bar(cx, "left");
        assert!(rx.try_recv().is_ok(), "any other bar key is pressed as before");

        // A click on a sent prompt puts it in the composer as if recalled: ↑ / ↓ go on from
        // it, and the draft under way comes back past the newest.
        up_down(cx, "cmd-a backspace d");
        let bubble = cx.debug_bounds("conversation-reuse-2").expect("the second prompt's bubble");
        cx.simulate_click(bubble.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(text(cx).as_deref(), Some("second"));
        assert!(composer_focused(&view, cx));
        up_down(cx, "up");
        assert_eq!(text(cx).as_deref(), Some("first"));
        up_down(cx, "down down down");
        assert_eq!(text(cx).as_deref(), Some("d"), "the draft is back past the newest");
        // The repeat of a prompt counts as the same step; typing after a click is the
        // human's own text and ↑ moves the caret.
        let again = cx.debug_bounds("conversation-reuse-3").expect("the repeated prompt");
        cx.simulate_click(again.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        up_down(cx, "! up");
        assert_eq!(text(cx).as_deref(), Some("second!"), "the caret was at the end");
        up_down(cx, "cmd-a backspace up");
        assert_eq!(text(cx).as_deref(), Some("third"), "the draft is empty again");
        assert!(rx.try_recv().is_err(), "nothing went to the program");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("Edit and send again"))), "{tree:#?}");
    }

    #[gpui::test]
    fn a_driven_view_steps_between_its_prompts(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        drain_words(&mut rx);
        let session = view.read_with(cx, |v, _| v.session);
        let mut entries = Vec::new();
        for turn in 0..8 {
            entries.push(user(&format!("prompt {turn}")));
            entries.push(assistant(&format!("answer {turn}\n\nwith a second paragraph")));
        }
        view.update(cx, |v, cx| {
            v.transcript_update(TranscriptUpdate { session, reset: true, entries }, cx);
        });
        cx.run_until_parked();
        let top = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.conversation().and_then(Conversation::top_entry))
        };
        let pinned = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.conversation().is_some_and(Conversation::pinned))
        };
        assert!(pinned(cx), "the list follows the tail to begin with");
        // ⌘↓ while following the tail has nowhere to go and keeps following.
        cx.simulate_keystrokes("cmd-down");
        cx.run_until_parked();
        assert!(pinned(cx));

        // ⌘↑ from the composer: the prompt above the viewport's top (or the top one, when the
        // reader is part-way through it), well past the first ones — sixteen entries overflow
        // the card — and it stops following the tail.
        cx.simulate_keystrokes("cmd-up");
        cx.run_until_parked();
        let (first, cut) = top(cx).expect("drawn");
        assert!(first % 2 == 0 && (4..16).contains(&first) && !cut, "{first} {cut}");
        assert!(!pinned(cx), "stepping up stops following the tail");
        // Every prompt is two entries up; the first stays put.
        cx.simulate_keystrokes("cmd-up");
        cx.run_until_parked();
        assert_eq!(top(cx), Some((first.saturating_sub(2), false)));
        for _ in 0..8 {
            cx.simulate_keystrokes("cmd-up");
            cx.run_until_parked();
        }
        assert_eq!(top(cx), Some((0, false)));
        // ⌘↓ steps down a prompt at a time; past the last one the list follows again.
        cx.simulate_keystrokes("cmd-down");
        cx.run_until_parked();
        assert_eq!(top(cx), Some((2, false)));
        for _ in 0..8 {
            cx.simulate_keystrokes("cmd-down");
            cx.run_until_parked();
        }
        assert!(pinned(cx), "past the last prompt the list follows the tail again");

        // The phone's armed ⌘ with the bar's ↑ is ⌘↑: it steps up a prompt and disarms.
        drain_words(&mut rx);
        view.update(cx, |v, cx| {
            v.set_sticky_command(true, cx);
            let up = Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: "up".into(),
                key_char: None,
            };
            v.press(up, cx);
        });
        cx.run_until_parked();
        assert_eq!(top(cx), Some((first, false)));
        assert!(!view.read_with(cx, |v, _| v.sticky_command()), "one press");
        assert!(rx.try_recv().is_err(), "nothing went to the program");

        // ⌘⇧C copies the newest answer, as the grid copies the newest block's output.
        cx.simulate_keystrokes("cmd-shift-c");
        cx.run_until_parked();
        let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()));
        assert_eq!(copied.as_deref(), Some("answer 7\n\nwith a second paragraph"));
    }

    #[gpui::test]
    fn a_driven_view_finds_in_its_conversation(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        drain_words(&mut rx);
        let session = view.read_with(cx, |v, _| v.session);
        let update = |reset: bool, entries: Vec<TranscriptEntry>| TranscriptUpdate {
            session,
            reset,
            entries,
        };
        view.update(cx, |v, cx| {
            v.transcript_update(
                update(
                    true,
                    vec![
                        user("where is the alpha path read?"),
                        assistant("In `beta.rs`, by `read_alpha`."),
                        tool("rg alpha"),
                        assistant("Done."),
                    ],
                ),
                cx,
            );
        });
        cx.run_until_parked();
        let hits = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.conversation().map(|c| (c.hits().0.to_vec(), c.hits().1)))
        };

        cx.simulate_keystrokes("cmd-f");
        cx.run_until_parked();
        let bar = cx.debug_bounds("terminal-search").expect("the bar is up");
        let header = cx.debug_bounds("conversation").expect("the conversation");
        assert!(bar.origin.y > header.origin.y, "the bar is a row in the card, not over it");
        cx.simulate_keystrokes("shift-a l p h a");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![], None)), "a capital is as typed: smart case");
        cx.simulate_keystrokes("cmd-a a l p h a");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 1, 2], Some(2))), "three entries, on the newest");
        assert!(drain_words(&mut rx).is_empty(), "nothing asked of the host");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let matches = |cx: &mut VisualTestContext| {
            let tree = cx.update(|window, _cx| crate::a11y::tree(window));
            tree.iter().find(|n| n.is("Label", Some("Matches"))).and_then(|n| n.value.clone())
        };
        assert_eq!(matches(cx).as_deref(), Some("3/3"), "the count reads the entries found");

        cx.simulate_keystrokes("cmd-g");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 1, 2], Some(0))), "⌘G wraps to the oldest");
        cx.simulate_keystrokes("shift-enter");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 1, 2], Some(2))), "⇧↩ back around");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 1, 2], Some(0))));

        // The transcript grows under the open bar: the hits follow, the reader stays put.
        view.update(cx, |v, cx| v.transcript_update(update(false, vec![user("alpha again")]), cx));
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 1, 2, 4], Some(0))));

        // `.*`: the needle is a regex; one that does not compile finds nothing and says so.
        let regex = cx.debug_bounds("terminal-search-regex").expect("the regex toggle");
        cx.simulate_click(regex.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 1, 2, 4], Some(3))), "the same hits, newest first");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_keystrokes("r e a d _ a l . h a");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![1], Some(0))));
        cx.simulate_keystrokes("(");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![], None)));
        assert!(view.read_with(cx, |v, _| v.search.as_ref().unwrap().invalid.is_some()));
        assert_eq!(matches(cx).as_deref(), Some("bad regex"));
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_keystrokes("z z z");
        cx.run_until_parked();
        assert_eq!(matches(cx).as_deref(), Some("none"), "a needle nothing holds");

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("terminal-search").is_none(), "closed");
        assert_eq!(hits(cx), Some((vec![], None)), "no tint left behind");
        assert!(composer_focused(&view, cx), "the caret is back in the composer");
    }

    /// A picture dropped from the desktop onto a driven card is attached like a pasted one,
    /// read off the UI thread; a file that is not a picture is refused by name; a shell card
    /// takes nothing, since the path is this machine's and the shell runs on the host.
    #[gpui::test]
    fn a_picture_dropped_on_a_driven_card_is_attached(cx: &mut TestAppContext) {
        assert_eq!(picture_type(std::path::Path::new("a/Shot.PNG")), Some("image/png"));
        assert_eq!(picture_type(std::path::Path::new("p.jpeg")), Some("image/jpeg"));
        assert_eq!(picture_type(std::path::Path::new("p.jpg")), Some("image/jpeg"));
        assert_eq!(picture_type(std::path::Path::new("p.gif")), Some("image/gif"));
        assert_eq!(picture_type(std::path::Path::new("p.webp")), Some("image/webp"));
        assert_eq!(picture_type(std::path::Path::new("p.tiff")), None);
        assert_eq!(picture_type(std::path::Path::new("png")), None, "no extension");

        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("shot.png");
        let small = encoded(64, 48, image::ImageFormat::Png);
        std::fs::write(&shot, &small).unwrap();
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "hue").unwrap();
        let gone = dir.path().join("gone.png");

        let (view, mut rx, cx) = terminal(cx);
        let notices = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&notices);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::Notice(text) = event {
                    seen.borrow_mut().push(text.clone());
                }
            })
            .detach();
        });
        let drop = |cx: &mut VisualTestContext, paths: &[&std::path::Path]| {
            let position = cx.debug_bounds("terminal").expect("the card").center();
            let paths = gpui::ExternalPaths(paths.iter().map(|p| p.to_path_buf()).collect());
            cx.simulate_event(gpui::FileDropEvent::Entered { position, paths });
            cx.simulate_event(gpui::FileDropEvent::Pending { position });
            cx.simulate_event(gpui::FileDropEvent::Submit { position });
            cx.run_until_parked();
        };

        // A shell card: nothing happens, not even a notice.
        drop(cx, &[&shot]);
        assert_eq!(view.read_with(cx, |v, _| (v.preparing(), v.attachments().len())), (0, 0));
        assert!(notices.borrow().is_empty(), "{notices:?}");

        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["follow:true"]);
        drop(cx, &[&shot, &notes, &gone]);
        let attached = view.read_with(cx, |v, _| v.attachments().to_vec());
        assert_eq!(attached.len(), 1, "{attached:?}");
        assert_eq!((attached[0].media_type.as_str(), &attached[0].data), ("image/png", &small));
        let noticed = notices.borrow().clone();
        assert_eq!(noticed.len(), 2, "{noticed:?}");
        assert_eq!(noticed[0], "notes.txt: not a picture (PNG, JPEG, GIF or WebP)");
        assert!(noticed[1].starts_with("gone.png: No such file"), "{noticed:?}");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let label = attachment_label(&attached[0]);
        assert!(
            tree.iter().any(|n| n.is("Button", Some(&format!("Remove picture 1: {label}")))),
            "the chip names the picture: {tree:#?}"
        );
    }

    /// ⌘V with a picture on the clipboard attaches it to the next prompt: a chip above the
    /// composer names it, its tap drops it, ↩ sends it with the text (or alone) and clears
    /// the chips; a text clipboard still pastes as text, and a type the model cannot read
    /// is not attached.
    #[gpui::test]
    fn a_picture_pasted_into_the_composer_goes_with_the_prompt(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        assert!(composer_focused(&view, cx));
        assert_eq!(drain_words(&mut rx), ["follow:true"]);
        let picture = |format, bytes: &[u8]| {
            gpui::ClipboardItem::new_image(&gpui::Image { format, bytes: bytes.to_vec(), id: 1 })
        };

        let notices = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&notices);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::Notice(text) = event {
                    seen.borrow_mut().push(text.clone());
                }
            })
            .detach();
        });
        let small = encoded(64, 48, image::ImageFormat::Png);
        let small_label = attachment_label(&slopty_proto::agent::Image {
            media_type: "image/png".to_owned(),
            data: small.clone(),
        });
        cx.update(|_, cx| cx.write_to_clipboard(picture(gpui::ImageFormat::Png, &small)));
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(
            tree.iter().any(|n| n.is("Button", Some(&format!("Remove picture 1: {small_label}")))),
            "the chip names the picture: {tree:#?}"
        );
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some(""), "nothing pasted as text");

        // A second picture, a screenshot too big for the model, is made fit off the UI
        // thread (a "preparing" chip meanwhile) and lands shrunk as a JPEG; then the first's
        // chip is tapped away.
        let big = encoded(3200, 1400, image::ImageFormat::Png);
        cx.update(|_, cx| cx.write_to_clipboard(picture(gpui::ImageFormat::Png, &big)));
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| (v.preparing(), v.attachments().len())), (0, 2));
        let fitted = view.read_with(cx, |v, _| v.attachments()[1].clone());
        assert_eq!(fitted.media_type, "image/jpeg", "shrunk and recompressed");
        let shrunk = image::load_from_memory(&fitted.data).expect("decodes");
        assert_eq!((shrunk.width(), shrunk.height()), (1568, 686), "long side clamped");
        let chip = cx.debug_bounds("composer-attachment-0").expect("the first chip");
        cx.simulate_click(chip.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let left: Vec<String> =
            view.read_with(cx, |v, _| v.attachments().iter().map(attachment_label).collect());
        assert_eq!(left, [attachment_label(&fitted)]);

        // Text still pastes as text; a TIFF is not something the model reads, and says so.
        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("hue".into())));
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some("hue"));
        cx.update(|_, cx| cx.write_to_clipboard(picture(gpui::ImageFormat::Tiff, &[0x4d; 10])));
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.attachments().len()), 1, "TIFF not attached");
        assert_eq!(notices.borrow().as_slice(), ["image/tiff is not a picture the agent can read"]);
        notices.borrow_mut().clear();

        // Past the cap the fifth is refused before any work is done.
        for _ in 0..3 {
            cx.update(|_, cx| cx.write_to_clipboard(picture(gpui::ImageFormat::Png, &small)));
            cx.simulate_keystrokes("cmd-v");
        }
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.attachments().len()), 4);
        cx.update(|_, cx| cx.write_to_clipboard(picture(gpui::ImageFormat::Png, &small)));
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.attachments().len()), 4);
        assert_eq!(notices.borrow().as_slice(), ["At most 4 pictures per prompt"]);
        for _ in 0..3 {
            view.update(cx, |v, cx| v.remove_attachment(1, cx));
        }
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.attachments().len()), 1);

        // ↩ sends the text with the picture and clears the chips.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let sent: Vec<(String, Vec<String>)> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|msg| match msg {
                ClientMsg::AgentSay { text, images, .. } => {
                    Some((text, images.iter().map(|i| i.media_type.clone()).collect()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(sent, [("hue".to_owned(), vec!["image/jpeg".to_owned()])]);
        assert!(view.read_with(cx, |v, _| v.attachments().is_empty()));
        assert!(cx.debug_bounds("composer-attachments").is_none(), "no chips left");

        // A picture alone is a prompt too (↩ once it is fit: the chip is what says so).
        cx.update(|_, cx| cx.write_to_clipboard(picture(gpui::ImageFormat::Png, &small)));
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let sent: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|msg| match msg {
                ClientMsg::AgentSay { text, images, .. } => {
                    Some(format!("{text}+{}", images.len()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(sent, ["+1"]);

        // The phone key bar's "paste" (no ⌘V on glass) reaches the same place: a picture is
        // attached, text lands in the composer, nothing goes to a session.
        cx.update(|_, cx| cx.write_to_clipboard(picture(gpui::ImageFormat::Png, &small)));
        view.update_in(cx, |v, window, cx| v.paste_clipboard(&Paste, window, cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.attachments().len()), 1);
        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("tint".into())));
        view.update_in(cx, |v, window, cx| v.paste_clipboard(&Paste, window, cx));
        cx.run_until_parked();
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some("tint"));
        assert!(drain_words(&mut rx).is_empty(), "no session paste from a driven card");
    }

    /// A driven agent's question is answered in place: the options are buttons, one tap on
    /// a single-select question is the whole answer (by request id, filed under the
    /// question's text); a multi-select question toggles and sends on Answer; typed text
    /// in the composer is the "Other" answer, not a prompt.
    #[gpui::test]
    fn a_driven_view_answers_a_question_in_place(cx: &mut TestAppContext) {
        use slopty_proto::agent::{Choice, Question};
        let (view, mut rx, cx) = terminal(cx);
        let answers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&answers);
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let TerminalViewEvent::AgentAnswered { request, allowed, answers, .. } = event {
                    let filed: Vec<String> =
                        answers.iter().map(|a| format!("{}={}", a.question, a.answer)).collect();
                    seen.borrow_mut().push(format!("{request}:{allowed}:{}", filed.join(";")));
                }
            })
            .detach();
        });
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        let _follow = drain_words(&mut rx);
        let choice = |label: &str, description: &str| Choice {
            label: label.to_owned(),
            description: description.to_owned(),
        };
        let colour = Question {
            text: "Which colour?".to_owned(),
            header: "Colour".to_owned(),
            multi: false,
            options: vec![choice("Red", "warm"), choice("Blue", "cool")],
        };
        let ask = |id: &str, questions: Vec<Question>| PermissionRequest {
            id: id.to_owned(),
            tool_use: "toolu_q".to_owned(),
            tool: "AskUserQuestion".to_owned(),
            summary: "Which colour?".to_owned(),
            detail: ToolDetail::Question { questions },
            always: None,
        };
        let blocked = || AgentStatus::Blocked(BlockReason::Question);
        view.update(cx, |v, cx| {
            v.agent_permission(ask("req_q1", vec![colour.clone()]), cx);
            v.set_agent_status(Some(blocked()), cx);
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        assert!(matches!(
            view.read_with(cx, |v, _| v.attention()),
            Some(Attention::Question { answered: false, .. })
        ));
        assert!(cx.debug_bounds("conversation-allow").is_none(), "a question has no Allow");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("Red"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Button", Some("Blue"))), "{tree:#?}");
        assert!(
            tree.iter().any(|n| n.is("Status", Some("Claude asks: Which colour?"))),
            "{tree:#?}"
        );
        let blue = cx.debug_bounds("question-option-0-1").expect("Blue is drawn");
        cx.simulate_click(blue.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*answers.borrow(), ["req_q1:true:Which colour?=Blue"], "one tap answers");
        assert!(drain_words(&mut rx).is_empty(), "nothing typed, nothing said");
        assert!(cx.debug_bounds("question-option-0-1").is_none(), "one tap, one answer");

        // Two questions, the second multi-select: taps pick and toggle, Answer sends both.
        let sizes = Question {
            text: "Which sizes?".to_owned(),
            header: "Sizes".to_owned(),
            multi: true,
            options: vec![choice("S", ""), choice("M", ""), choice("L", "")],
        };
        view.update(cx, |v, cx| {
            v.set_agent_status(Some(AgentStatus::Working), cx);
            v.agent_permission(ask("req_q2", vec![colour.clone(), sizes]), cx);
            v.set_agent_status(Some(blocked()), cx);
        });
        cx.run_until_parked();
        let answer = cx.debug_bounds("question-answer").expect("Answer is drawn");
        cx.simulate_click(answer.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(answers.borrow().len(), 1, "nothing sent before every question has a pick");
        for id in ["question-option-0-0", "question-option-1-0", "question-option-1-2"] {
            let b = cx.debug_bounds(id).expect(id);
            cx.simulate_click(b.center(), gpui::Modifiers::default());
            cx.run_until_parked();
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.chosen().to_vec()),
            [vec!["Red".to_owned()], vec!["S".to_owned(), "L".to_owned()]]
        );
        let s = cx.debug_bounds("question-option-1-0").expect("S");
        cx.simulate_click(s.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.chosen()[1].clone()), ["L"], "toggled off");
        assert_eq!(answers.borrow().len(), 1, "a multi-select question waits for Answer");
        let answer = cx.debug_bounds("question-answer").expect("Answer is drawn");
        cx.simulate_click(answer.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(answers.borrow()[1], "req_q2:true:Which colour?=Red;Which sizes?=L");

        // Typed text is the answer to the question, not a prompt.
        view.update(cx, |v, cx| {
            v.set_agent_status(Some(AgentStatus::Working), cx);
            v.agent_permission(ask("req_q3", vec![colour]), cx);
            v.set_agent_status(Some(blocked()), cx);
        });
        cx.run_until_parked();
        assert!(composer_focused(&view, cx), "a question puts the caret in the composer");
        cx.simulate_keystrokes("g r e e n enter");
        cx.run_until_parked();
        assert_eq!(answers.borrow()[2], "req_q3:true:Which colour?=green");
        assert!(drain_words(&mut rx).is_empty(), "not said as a prompt");
    }

    /// A driven view shows what the agent said about itself over the list — the model as a
    /// chip whose menu switches it, the permission mode as a chip that cycles, the turns and
    /// the cost — and its composer completes the slash commands the agent announced: `/`
    /// lists the matches, ↓ moves, Tab takes one, Esc hides the list without interrupting.
    #[gpui::test]
    fn a_driven_view_shows_the_agent_and_retunes_it(cx: &mut TestAppContext) {
        let (view, mut rx, cx) = terminal(cx);
        view.update(cx, TerminalView::set_driven);
        cx.run_until_parked();
        drain_words(&mut rx);
        assert!(cx.debug_bounds("conversation-header").is_some(), "the header is there at once");
        view.update(cx, |v, cx| {
            v.agent_info(
                AgentInfo {
                    agent_session: Some("s1".to_owned()),
                    cwd: Some("/w/slopty/.claude/worktrees/fix-x".to_owned()),
                    model: Some("claude-sonnet-5".to_owned()),
                    permission_mode: Some("default".to_owned()),
                    slash_commands: vec![
                        SlashCommand {
                            name: "/compact".to_owned(),
                            description: "Summarise the conversation".to_owned(),
                            hint: "[instructions]".to_owned(),
                        },
                        SlashCommand::named("clear"),
                        SlashCommand {
                            name: "/cost".to_owned(),
                            description: "Show the total cost".to_owned(),
                            hint: String::new(),
                        },
                    ],
                    turns: 2,
                    cost_micro_usd: 12_500,
                    usage: Some(slopty_proto::agent::Usage {
                        limited: false,
                        five_hour: Some(slopty_proto::agent::UsageWindow {
                            percent: 23,
                            resets_at: 1_789_195_800,
                        }),
                        seven_day: Some(slopty_proto::agent::UsageWindow {
                            percent: 74,
                            resets_at: 1_789_257_600,
                        }),
                    }),
                    context: Some(slopty_proto::agent::Context {
                        tokens: 31_000,
                        window: Some(200_000),
                    }),
                },
                cx,
            );
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Status", Some("Usage: 5h 23% · 7d 74%"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Status", Some("Context: ctx 16%"))), "{tree:#?}");
        let context = |tokens, window| {
            conversation::context_label(&slopty_proto::agent::Context { tokens, window })
        };
        assert_eq!(context(900, None), "ctx 900");
        assert_eq!(context(31_000, None), "ctx 31k");
        assert_eq!(context(1_250_000, None), "ctx 1.3M");
        assert_eq!(context(200_000, Some(200_000)), "ctx 100%");
        assert_eq!(context(1, Some(200_000)), "ctx 1%");
        assert!(tree.iter().any(|n| n.is("Button", Some("Model: sonnet-5"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Button", Some("Permission mode: Ask"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Button", Some("Worktree: fix-x"))), "{tree:#?}");
        assert_eq!(conversation::worktree_name("/w/slopty"), None);
        assert_eq!(conversation::worktree_name("/w/.claude/worktrees/a/src"), Some("a"));
        assert_eq!(conversation::cost_label(12_500), "1¢");
        assert_eq!(conversation::cost_label(1_234_567), "$1.23");

        // The model chip opens the menu; a model in it is asked of the host, and the chip
        // only changes when the host confirms.
        let chip = cx.debug_bounds("conversation-model").expect("the model chip");
        cx.simulate_click(chip.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation-model-opus").is_some(), "the menu opened");
        cx.simulate_click(chip.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation-model-opus").is_none(), "the chip again closes it");
        cx.simulate_click(chip.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let opus = cx.debug_bounds("conversation-model-opus").expect("the menu opened");
        cx.simulate_click(opus.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["set:model=opus"]);
        assert!(cx.debug_bounds("conversation-models").is_none(), "one pick closes the menu");
        assert_eq!(
            view.read_with(cx, |v, _| v.info().model.clone()).as_deref(),
            Some("claude-sonnet-5")
        );
        view.update(cx, |v, cx| {
            let mut info = v.info().clone();
            info.model = Some("opus".to_owned());
            v.agent_info(info, cx);
        });
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("Model: Opus 5"))), "{tree:#?}");

        // The mode chip asks for the next mode.
        let chip = cx.debug_bounds("conversation-mode").expect("the mode chip");
        cx.simulate_click(chip.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["set:mode=acceptEdits"]);
        assert_eq!(conversation::next_mode("plan"), "default", "the cycle wraps");

        // Slash completion in the composer.
        assert!(composer_focused(&view, cx));
        cx.simulate_keystrokes("/ c");
        cx.run_until_parked();
        let completions =
            |cx: &mut VisualTestContext| view.read_with(cx, TerminalView::completions);
        assert_eq!(completions(cx), ["/compact", "/clear", "/cost"]);
        assert!(cx.debug_bounds("conversation-completions").is_some(), "listed above the composer");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        for label in [
            "/compact [instructions] — Summarise the conversation",
            "/clear",
            "/cost — Show the total cost",
        ] {
            assert!(tree.iter().any(|n| n.is("ListBoxOption", Some(label))), "{label}: {tree:#?}");
        }
        cx.simulate_keystrokes("down tab");
        cx.run_until_parked();
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some("/clear "), "Tab takes the selected one");
        assert!(completions(cx).is_empty(), "a completed command lists nothing");
        assert!(drain_words(&mut rx).is_empty(), "nothing was sent");

        view.update_in(cx, |v, window, cx| {
            if let Some(c) = v.conversation_mut() {
                let _taken = c.take_composer_text(window, cx);
            }
        });
        cx.simulate_keystrokes("/ c o");
        cx.run_until_parked();
        assert_eq!(completions(cx), ["/compact", "/cost"]);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(completions(cx).is_empty(), "Esc hides the list");
        assert!(drain_words(&mut rx).is_empty(), "and does not interrupt the agent");
        cx.simulate_keystrokes("s t");
        cx.run_until_parked();
        assert!(completions(cx).is_empty(), "the one match is the text itself");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["say:/cost"], "↩ sends the command as a prompt");

        // `@` completion: the word is asked of the host once per query, the answer lists
        // the paths while the word is still that query, Tab replaces the word alone.
        view.update_in(cx, |v, window, cx| {
            if let Some(c) = v.conversation_mut() {
                let _taken = c.take_composer_text(window, cx);
            }
        });
        cx.simulate_keystrokes("s e e space @ m a");
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["files:m", "files:ma"], "asked as the word grows");
        view.update(cx, |v, cx| {
            v.files("m".to_owned(), vec!["Makefile".to_owned()], cx);
            v.files("ma".to_owned(), vec!["src/main.rs".to_owned(), "docs/manual/".to_owned()], cx);
        });
        cx.run_until_parked();
        assert_eq!(completions(cx), ["@src/main.rs", "@docs/manual/"], "the current answer only");
        // The word grows past the answer: the stale list hides until the host answers again.
        cx.simulate_keystrokes("i");
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["files:mai"]);
        assert!(completions(cx).is_empty(), "an answer for `ma` is not one for `mai`");
        view.update(cx, |v, cx| v.files("mai".to_owned(), vec!["src/main.rs".to_owned()], cx));
        cx.run_until_parked();
        assert_eq!(completions(cx), ["@src/main.rs"]);
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some("see @src/main.rs "), "the word alone is replaced");
        assert!(completions(cx).is_empty(), "the space ends the word");
        assert!(drain_words(&mut rx).is_empty(), "no word: nothing asked");
        // A directory gets no space: the word goes on, and the host is asked for what is in it.
        cx.simulate_keystrokes("@ d");
        cx.run_until_parked();
        assert_eq!(drain_words(&mut rx), ["files:d"]);
        view.update(cx, |v, cx| v.files("d".to_owned(), vec!["docs/".to_owned()], cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        let text = view.read_with(cx, |v, cx| v.conversation().map(|c| c.composer_text(cx)));
        assert_eq!(text.as_deref(), Some("see @src/main.rs @docs/"), "no space after a directory");
        assert_eq!(drain_words(&mut rx), ["files:docs/"], "its contents are asked for");
        assert_eq!(conversation::file_query("look at @src/ma"), Some("src/ma"));
        assert_eq!(conversation::file_query("@"), Some(""));
        assert_eq!(conversation::file_query("mail@x y"), None);
        let named =
            |names: &[&str]| names.iter().map(|n| SlashCommand::named(n)).collect::<Vec<_>>();
        let names =
            |matches: Vec<SlashCommand>| matches.into_iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(
            names(conversation::slash_matches("/C", &named(&["/compact", "/clear"]))),
            ["/compact", "/clear"],
            "matching ignores case"
        );
        assert!(conversation::slash_matches("/co x", &named(&["/compact"])).is_empty());
        assert!(conversation::slash_matches("hello", &named(&["/help"])).is_empty());
        assert!(conversation::slash_matches("hello x", &named(&["/help"])).is_empty());
        assert!(
            conversation::slash_matches("/Compact", &named(&["/compact"])).is_empty(),
            "the only match typed out in full leaves nothing to complete"
        );
        assert_eq!(
            names(conversation::slash_matches("/comp", &named(&["/compact"]))),
            ["/compact"],
            "one match not yet typed out still completes"
        );
        let label = |bytes: usize| {
            attachment_label(&slopty_proto::agent::Image {
                media_type: "image/png".to_owned(),
                data: vec![0; bytes],
            })
        };
        assert_eq!(label(999), "PNG · 999 B");
        assert_eq!(label(1024), "PNG · 1 KB");
        assert_eq!(label(1024 * 1024 - 1), "PNG · 1023 KB");
        assert_eq!(label(1024 * 1024), "PNG · 1.0 MB");
        assert_eq!(label(1024 * 1024 * 3 / 2), "PNG · 1.5 MB");
        let many: Vec<String> = (0..12).map(|i| format!("/cmd{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            conversation::slash_matches("/", &named(&many)).len(),
            conversation::SLASH_MAX,
            "the list is capped"
        );
        assert_eq!(conversation::model_label("claude-fable-5-1"), "fable-5-1");
        assert_eq!(conversation::model_label("fable"), "Fable 5.1");
    }
}
