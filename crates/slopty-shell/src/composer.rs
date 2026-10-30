// Derived from zeron (MIT, github.com/zeronsh/zeron @ eabad36), see THIRD_PARTY_NOTICES.md
//! The shell's text field: a hand-rolled multiline text input (adapted from gpui's
//! `examples/input.rs`) with IME, undo, word and line motion, drag selection and auto-grow.
//!
//! TUI-first (`docs/decisions/agents.md`): every text field in the shell is one of these, plain
//! words, and the person writes to an agent in its own TUI.
//!
//! Pure decision logic (auto-grow math, caret and scroll math) lives in free functions with unit
//! tests; the gpui element only feeds them measurements.

use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Role, ScrollEvent, SharedString, Style, Task, TextRun, TextStyle,
    UTF16Selection, UnderlineStyle, Window, WrappedLine, div, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Constants + pure decision logic
// ---------------------------------------------------------------------------

/// The tallest a field's words grow before they scroll.
const INPUT_MAX_HEIGHT: f32 = 240.0;

/// Input text metrics: `text-[14px] leading-relaxed` = 14 × 1.625 = 22.75.
pub const INPUT_LINE_HEIGHT: f32 = 22.75;
/// Input text size, in points.
pub const INPUT_TEXT_SIZE: f32 = 14.0;

/// A compact ramp; the glyph-ascent inset keeps the clip edge invisible.
const INPUT_FADE_BAND: f32 = 12.0;

/// Drag-selection autoscroll runs at the display-friendly 60fps cadence.
pub const DRAG_SCROLL_FRAME_MS: u64 = 16;

/// Caret blink half-period (standard textarea cadence: ~500ms on / 500ms off).
pub const CARET_BLINK_MS: u64 = 500;

fn input_max_scroll(content_height: f32, viewport_height: f32) -> f32 {
    (content_height - viewport_height).max(0.0)
}

/// Only settled overflow gets a scroll fade. The animated viewport can be
/// smaller for a few frames while an otherwise fitting draft grows into it.
fn input_overflow_edges(
    content_height: f32,
    settled_height: f32,
    visible_height: f32,
    scroll_top: f32,
) -> (bool, bool) {
    if input_max_scroll(content_height, settled_height) <= 1.0 {
        return (false, false);
    }
    let max_scroll = input_max_scroll(content_height, visible_height);
    (scroll_top > 1.0, scroll_top < max_scroll - 1.0)
}

/// During the reveal, stop at a complete row boundary instead of slicing
/// glyphs with a moving clip. Scrolling offsets the row grid inside the box.
fn input_reveal_height(visible: f32, scroll: f32, line_height: f32, resizing: bool) -> f32 {
    if !resizing {
        return visible;
    }
    let row_end = ((scroll + visible + 0.001) / line_height).floor() * line_height;
    (row_end - scroll).clamp(0.0, visible)
}

/// Apply GPUI's wheel delta to a top-origin input offset. Positive deltas mean
/// scrolling toward the start, matching gpui's built-in list/div behavior.
fn input_scroll_offset(
    current: f32,
    delta_y: f32,
    content_height: f32,
    viewport_height: f32,
) -> f32 {
    (current - delta_y).clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// Minimally adjust the viewport so the caret row is fully visible.
fn input_scroll_offset_for_cursor(
    current: f32,
    cursor_top: f32,
    cursor_height: f32,
    content_height: f32,
    viewport_height: f32,
    settled_height: Option<f32>,
) -> f32 {
    // Resize the reveal, not the scroll position: existing text stays fixed
    // relative to the input origin throughout the height animation.
    let viewport_height = settled_height.unwrap_or(viewport_height);
    let mut next = current;
    if cursor_top < next {
        next = cursor_top;
    } else if cursor_top + cursor_height > next + viewport_height {
        next = cursor_top + cursor_height - viewport_height;
    }
    next.clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// Pointer selection granularity; multi-click drags retain the initial unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PressIntent {
    Word,
    Line,
    ExtendSelection,
    PlaceCaret,
}

const fn press_intent(click_count: usize, shift: bool) -> PressIntent {
    if click_count >= 3 {
        PressIntent::Line
    } else if click_count == 2 {
        PressIntent::Word
    } else if shift {
        PressIntent::ExtendSelection
    } else {
        PressIntent::PlaceCaret
    }
}

fn word_range(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    text.split_word_bound_indices()
        .map(|(at, word)| at..at.saturating_add(word.len()))
        .find(|range| range.contains(&offset))
        .unwrap_or(offset..offset)
}

/// The byte index in `line` of the glyph a soft wrap breaks before, which opens the next row.
fn wrap_index(line: &WrappedLine, boundary: &gpui::WrapBoundary) -> Option<usize> {
    line.runs().get(boundary.run_ix)?.glyphs.get(boundary.glyph_ix).map(|glyph| glyph.index)
}

/// Per-frame drag-selection scroll. Distance increases speed, capped at one
/// text row per frame so crossing the input boundary never causes a jump.
fn input_drag_scroll_delta(
    pointer_y: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    line_height: f32,
) -> f32 {
    let distance = if pointer_y < viewport_top {
        pointer_y - viewport_top
    } else if pointer_y > viewport_bottom {
        pointer_y - viewport_bottom
    } else {
        return 0.0;
    };
    distance.signum() * (distance.abs() * 0.2).clamp(1.0, line_height)
}

fn escape_dismisses_completion(key: &str, completion_open: bool) -> bool {
    key == "escape" && completion_open
}

/// The field's key actions, bound in [`init`].
mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        composer,
        [
            /// Delete the selection, or the character before the caret.
            Backspace,
            /// Delete the selection, or the character after the caret.
            Delete,
            /// Move the caret a character left, or to the selection's start.
            Left,
            /// Move the caret a character right, or to the selection's end.
            Right,
            /// Move the caret a row up.
            Up,
            /// Move the caret a row down.
            Down,
            /// Extend the selection a character left.
            SelectLeft,
            /// Extend the selection a character right.
            SelectRight,
            /// Extend the selection a row up.
            SelectUp,
            /// Extend the selection a row down.
            SelectDown,
            /// Select all the words.
            SelectAll,
            /// Move the caret to its line's start.
            Home,
            /// Move the caret to its line's end.
            End,
            /// Extend the selection to its line's start.
            SelectHome,
            /// Extend the selection to its line's end.
            SelectEnd,
            /// Move the caret to the words' start.
            DocStart,
            /// Move the caret to the words' end.
            DocEnd,
            /// Extend the selection to the words' start.
            SelectDocStart,
            /// Extend the selection to the words' end.
            SelectDocEnd,
            /// Move the caret to the previous word's start.
            WordLeft,
            /// Move the caret to the next word's end.
            WordRight,
            /// Extend the selection to the previous word's start.
            SelectWordLeft,
            /// Extend the selection to the next word's end.
            SelectWordRight,
            /// Delete back to the previous word's start.
            DeleteWordLeft,
            /// Delete on to the next word's end.
            DeleteWordRight,
            /// Delete back to the line's start.
            DeleteToLineStart,
            /// Delete on to the line's end.
            DeleteToLineEnd,
            /// Copy the selection.
            Copy,
            /// Cut the selection.
            Cut,
            /// Paste the clipboard's words.
            Paste,
            /// Insert a line break.
            Newline,
            /// Take the owner's picked row while it shows one, or submit the words.
            Submit,
            /// Undo the last edit.
            Undo,
            /// Redo the last undone edit.
            Redo,
            /// Take the owner's picked row, or move on to the next field.
            MentionTab,
        ]
    );
}
pub use actions::{
    Backspace, Copy, Cut, Delete, DeleteToLineEnd, DeleteToLineStart, DeleteWordLeft,
    DeleteWordRight, DocEnd, DocStart, Down, End, Home, Left, MentionTab, Newline, Paste, Redo,
    Right, SelectAll, SelectDocEnd, SelectDocStart, SelectDown, SelectEnd, SelectHome, SelectLeft,
    SelectRight, SelectUp, SelectWordLeft, SelectWordRight, Submit, Undo, Up, WordLeft, WordRight,
};

/// How long a run of single-character edits keeps merging into one undo step.
/// A pause longer than this starts a fresh step, so undo rewinds in the
/// bursts the user actually typed rather than one character at a time.
const UNDO_COALESCE: Duration = Duration::from_millis(700);

/// Cap on retained undo steps — a long-lived composer must not grow forever.
const UNDO_LIMIT: usize = 200;

/// A restorable point in the input's history: text plus where the caret and
/// selection sat when the edit landed.
#[derive(Clone)]
struct EditSnapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
    caret_affinity: CaretAffinity,
}

/// Direction of the last edit — a run only merges with edits of its own kind.
#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Insert,
    Delete,
}

const GENERIC_COMPOSER_CONTEXT: &str = "Composer";
const PALETTE_SEARCH_CONTEXT: &str = "PaletteSearch";

fn input_bindings(context: &'static str, cx: &App) -> Vec<KeyBinding> {
    let ctx = Some(context);
    let mut bindings = vec![
        KeyBinding::new("tab", MentionTab, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("shift-backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        // macOS line/document motion — a laptop keyboard has no home/end keys,
        // so Cmd+arrow is the only way users reach either edge.
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("cmd-up", DocStart, ctx),
        KeyBinding::new("cmd-down", DocEnd, ctx),
        KeyBinding::new("shift-cmd-left", SelectHome, ctx),
        KeyBinding::new("shift-cmd-right", SelectEnd, ctx),
        KeyBinding::new("shift-cmd-up", SelectDocStart, ctx),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, ctx),
        // Line-edge deletion (Cmd+Delete on macOS).
        KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
        KeyBinding::new("cmd-delete", DeleteToLineEnd, ctx),
    ];
    for prefix in ["cmd", "ctrl"] {
        bindings.extend(crate::keys::binding(&format!("{prefix}-z"), Undo, ctx, cx));
        bindings.extend(crate::keys::binding(&format!("shift-{prefix}-z"), Redo, ctx, cx));
    }
    // Word-level editing: Option on Apple keyboards, Ctrl elsewhere (`MOD_IS_COMMAND`).
    let word_edit_prefix = if crate::keys::MOD_IS_COMMAND {
        // an iPad's ⌥ too
        "alt"
    } else {
        "ctrl"
    };
    bindings.extend(crate::keys::binding(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        ctx,
        cx,
    ));
    bindings.extend(crate::keys::binding(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        ctx,
        cx,
    ));
    bindings.extend(crate::keys::binding(&format!("{word_edit_prefix}-left"), WordLeft, ctx, cx));
    bindings.extend(crate::keys::binding(&format!("{word_edit_prefix}-right"), WordRight, ctx, cx));
    bindings.extend(crate::keys::binding(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        ctx,
        cx,
    ));
    bindings.extend(crate::keys::binding(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        ctx,
        cx,
    ));
    for prefix in ["cmd", "ctrl"] {
        bindings.extend(crate::keys::binding(&format!("{prefix}-a"), SelectAll, ctx, cx));
        bindings.extend(crate::keys::binding(&format!("{prefix}-c"), Copy, ctx, cx));
        bindings.extend(crate::keys::binding(&format!("{prefix}-x"), Cut, ctx, cx));
        bindings.extend(crate::keys::binding(&format!("{prefix}-v"), Paste, ctx, cx));
    }
    bindings
}

/// Bind the text fields' keymap. Call once at app boot.
pub fn init(cx: &mut App) {
    let mut generic_bindings = input_bindings(GENERIC_COMPOSER_CONTEXT, cx);
    generic_bindings.push(KeyBinding::new("enter", Submit, Some(GENERIC_COMPOSER_CONTEXT)));

    let word_edit_prefix = if crate::keys::MOD_IS_COMMAND {
        // an iPad's ⌥ too
        "alt"
    } else {
        "ctrl"
    };
    // Palette-search context: TEXT-EDITING keys only. gpui dispatches matched
    // keybindings BEFORE raw key listeners (window.rs `dispatch_key_event`),
    // so anything bound here can never reach a palette's `on_key_down` —
    // navigation keys (up/down/left/right/enter) are deliberately unbound and
    // bubble to the palette frame instead.
    let palette = Some(PALETTE_SEARCH_CONTEXT);
    let mut palette_bindings = vec![
        KeyBinding::new("backspace", Backspace, palette),
        KeyBinding::new("shift-backspace", Backspace, palette),
        KeyBinding::new("delete", Delete, palette),
        KeyBinding::new("home", Home, palette),
        KeyBinding::new("end", End, palette),
        KeyBinding::new("shift-left", SelectLeft, palette),
        KeyBinding::new("shift-right", SelectRight, palette),
        // Modifier-qualified motion is safe here: the palette's own navigation
        // uses BARE arrows/enter, which stay unbound and bubble to its frame.
        KeyBinding::new("cmd-left", Home, palette),
        KeyBinding::new("cmd-right", End, palette),
        KeyBinding::new("shift-cmd-left", SelectHome, palette),
        KeyBinding::new("shift-cmd-right", SelectEnd, palette),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, palette),
    ];
    palette_bindings.extend(crate::keys::binding(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        palette,
        cx,
    ));
    palette_bindings.extend(crate::keys::binding(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        palette,
        cx,
    ));
    palette_bindings.extend(crate::keys::binding(
        &format!("{word_edit_prefix}-left"),
        WordLeft,
        palette,
        cx,
    ));
    palette_bindings.extend(crate::keys::binding(
        &format!("{word_edit_prefix}-right"),
        WordRight,
        palette,
        cx,
    ));
    palette_bindings.extend(crate::keys::binding(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        palette,
        cx,
    ));
    palette_bindings.extend(crate::keys::binding(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        palette,
        cx,
    ));
    for prefix in ["cmd", "ctrl"] {
        palette_bindings.extend(crate::keys::binding(
            &format!("{prefix}-a"),
            SelectAll,
            palette,
            cx,
        ));
        palette_bindings.extend(crate::keys::binding(&format!("{prefix}-c"), Copy, palette, cx));
        palette_bindings.extend(crate::keys::binding(&format!("{prefix}-x"), Cut, palette, cx));
        palette_bindings.extend(crate::keys::binding(&format!("{prefix}-v"), Paste, palette, cx));
        palette_bindings.extend(crate::keys::binding(&format!("{prefix}-z"), Undo, palette, cx));
        palette_bindings.extend(crate::keys::binding(
            &format!("shift-{prefix}-z"),
            Redo,
            palette,
            cx,
        ));
    }
    cx.bind_keys(palette_bindings);
    cx.bind_keys(generic_bindings);
}

/// Events the field's owner listens for. The `Mention…` ones drive the owner's list of rows
/// under the field (a palette's or a picker's), while it says one shows
/// ([`ComposerInput::set_mention_controls`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerInputEvent {
    /// Enter submitted the words.
    Submitted,
    /// The words changed.
    Edited,
    /// The caret or the selection moved.
    CursorMoved,
    /// The field's laid-out height or its scroll changed.
    ViewportChanged,
    /// Up or Down moved the owner's row pick by this many rows.
    MentionNavigate(isize),
    /// Enter or Tab took the owner's picked row.
    MentionAccept,
    /// Escape closed the owner's rows.
    MentionDismiss,
}

/// Shaping inputs excluding mutable viewport and selection geometry.
#[derive(Clone, PartialEq)]
struct InputLayoutKey {
    width: Pixels,
    font: gpui::Font,
    font_size: Pixels,
    color: gpui::Hsla,
    marked_range: Option<Range<usize>>,
    placeholder: SharedString,
}

/// A soft-wrap boundary is both the previous row's end and the next row's
/// start. Keep the visual side of the caret separately from its source offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CaretAffinity {
    Upstream,
    #[default]
    Downstream,
}

/// Multiline input entity: content + selection + IME marked text + measured
/// layout (wrapped lines) for mouse mapping and auto-grow.
pub struct ComposerInput {
    /// Key context for the binding map ("Composer", or "`PaletteSearch`" for
    /// palette filters whose navigation keys must bubble).
    key_context: &'static str,
    accessibility_role: Role,
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    /// What assistive tech calls the field when its placeholder is not its name (a form
    /// field whose label shows above it); none names it by its placeholder.
    name: Option<SharedString>,
    selected_range: Range<usize>,
    selection_reversed: bool,
    caret_affinity: CaretAffinity,
    marked_range: Option<Range<usize>>,
    /// Retained through vertical moves across short rows; other moves reset it.
    preferred_column: Option<Pixels>,
    is_selecting: bool,
    drag_position: Option<Point<Pixels>>,
    drag_generation: u64,
    drag_unit: Option<(PressIntent, Range<usize>)>,
    drag_autoscroll_active: bool,
    /// Vertical scroll inside the input once content exceeds the max height.
    scroll_top: f32,
    /// Visible content budget supplied by the animated composer.
    viewport_height: Option<f32>,
    /// Final content budget, excluding temporary overflow during a resize.
    settled_viewport_height: Option<f32>,
    resizing: bool,
    overflow_top_padding: f32,
    needs_measure: bool,
    last_layout_key: Option<InputLayoutKey>,
    last_notified_layout: Option<(Pixels, f32)>,
    max_ascent: f32,
    #[cfg(test)]
    layout_rebuilds: usize,
    /// The runs of the last rebuilt layout; the test text system shapes
    /// every run alike, so tests check the requested fonts here.
    #[cfg(test)]
    last_runs: Vec<TextRun>,
    /// Normally keeps the caret visible through edits and rewraps. Manual
    /// wheel scrolling pauses it until the next caret move or edit.
    follow_cursor: bool,
    text_size: f32,
    configured_line_height: f32,
    single_line: bool,
    scroll_left: f32,
    // -- measured state (written during layout/paint) --
    last_lines: Vec<WrappedLine>,
    line_starts: Vec<usize>,
    last_bounds: Option<Bounds<Pixels>>,
    line_height: Pixels,
    content_height: f32,
    max_line_width: f32,
    last_width: f32,
    /// Inline completion preview: painted in faint ink after the text while
    /// the caret sits at the end (palette tab-completion). Owned by the
    /// wrapper — it recomputes and re-sets this on every render pass, so the
    /// input never has to know what the completion means.
    ghost: Option<SharedString>,
    /// Bumped once per `layout_text` pass — the flip logic uses it to apply at
    /// most one compact↔expanded flip per layout (a flip is only re-evaluated
    /// after the input has been measured in the new mode).
    layout_epoch: u64,
    display_is_placeholder: bool,
    // The phase is state the driver flips, never read off the wall clock as it paints,
    // so a frame drawn again shows the caret as the frame it replaces did.
    /// Whether the caret shows in this half-period of its blink: on again at every keystroke
    /// and caret move, so the caret is solid while typing and blinks at [`CARET_BLINK_MS`]
    /// when idle.
    caret_on: bool,
    /// Half-period driver that flips [`Self::caret_on`], alive only while the input is
    /// focused; restarted with each reset, so its half-periods start there.
    blink_task: Option<Task<()>>,
    // -- undo history --
    undo_stack: Vec<EditSnapshot>,
    redo_stack: Vec<EditSnapshot>,
    /// Kind, trailing offset, and time of the last edit — the merge test that
    /// decides whether the next edit extends the current undo step.
    last_edit: Option<(EditKind, usize, Instant)>,
    /// The wrapper owns mention state; this only redirects bound keys while a
    /// mention token is active, keeping input focus and native text editing.
    mention_open: bool,
    mention_has_selection: bool,
}

impl ComposerInput {
    /// A multiline field showing `placeholder` while empty, under the generic composer keys.
    pub fn new(placeholder: impl Into<SharedString>, cx: &Context<Self>) -> Self {
        Self::with_context(placeholder, GENERIC_COMPOSER_CONTEXT, cx)
    }

    /// An input in a custom KEY context — palettes use `"PaletteSearch"`,
    /// whose keymap binds only text-editing keys so navigation keys bubble to
    /// the surrounding frame (see `init`).
    pub fn with_context(
        placeholder: impl Into<SharedString>,
        key_context: &'static str,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            key_context,
            accessibility_role: Role::MultilineTextInput,
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            name: None,
            selected_range: 0..0,
            selection_reversed: false,
            caret_affinity: CaretAffinity::Downstream,
            marked_range: None,
            preferred_column: None,
            is_selecting: false,
            drag_position: None,
            drag_generation: 0,
            drag_unit: None,
            drag_autoscroll_active: false,
            scroll_top: 0.0,
            viewport_height: None,
            settled_viewport_height: None,
            resizing: false,
            overflow_top_padding: 0.0,
            needs_measure: true,
            last_layout_key: None,
            last_notified_layout: None,
            max_ascent: INPUT_TEXT_SIZE,
            #[cfg(test)]
            layout_rebuilds: 0,
            #[cfg(test)]
            last_runs: Vec::new(),
            follow_cursor: true,
            text_size: INPUT_TEXT_SIZE,
            configured_line_height: INPUT_LINE_HEIGHT,
            single_line: false,
            scroll_left: 0.0,
            last_lines: Vec::new(),
            line_starts: vec![0],
            last_bounds: None,
            line_height: px(INPUT_LINE_HEIGHT),
            content_height: INPUT_LINE_HEIGHT,
            max_line_width: 0.0,
            last_width: 0.0,
            ghost: None,
            layout_epoch: 0,
            display_is_placeholder: true,
            caret_on: true,
            blink_task: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            mention_open: false,
            mention_has_selection: false,
        }
    }

    /// Override the text metrics for compact one-line surfaces such as
    /// toolbar searches without changing the main composer typography.
    #[must_use]
    pub const fn with_text_metrics(mut self, text_size: f32, line_height: f32) -> Self {
        self.text_size = text_size;
        self.configured_line_height = line_height;
        self.line_height = px(line_height);
        self.content_height = line_height;
        self
    }

    /// Keeps the field to one row that scrolls sideways; a line break set or pasted becomes a
    /// space.
    #[must_use]
    pub const fn with_single_line(mut self) -> Self {
        self.single_line = true;
        self
    }

    /// Names the field for assistive tech apart from its placeholder.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<SharedString>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The role assistive tech reads the field as: a multiline text input unless said.
    #[must_use]
    pub const fn with_accessibility_role(mut self, role: Role) -> Self {
        self.accessibility_role = role;
        self
    }

    /// Reset the caret blink phase (solid again) — called on every edit and
    /// caret move, matching textarea behavior.
    fn reset_blink(&mut self) {
        self.caret_on = true;
        self.blink_task = None;
    }

    /// Caret paint gate: focused input in an active window, in the "on" blink
    /// phase. Also (re)arms the half-period repaint driver while focused, and
    /// drops it on blur so an unfocused input schedules no frames.
    fn caret_shown(&mut self, window: &Window, cx: &Context<Self>) -> bool {
        let focused = self.focus_handle.is_focused(window);
        if !focused || !window.is_window_active() {
            self.blink_task = None;
            return false;
        }
        // Reduce Motion holds the caret still, as iOS keeps Prefer Non-Blinking Cursor
        // beside it under Motion: on while focused, and nothing wakes the app to flip it.
        if crate::motion::reduced_motion(cx) {
            self.blink_task = None;
            return true;
        }
        if self.blink_task.is_none() {
            self.blink_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(CARET_BLINK_MS)).await;
                    let flipped = this.update(cx, |this: &mut Self, cx| {
                        this.caret_on = !this.caret_on;
                        cx.notify();
                    });
                    if flipped.is_err() {
                        break;
                    }
                }
            }));
        }
        self.caret_on
    }

    /// Where the input was last drawn.
    pub(crate) const fn drawn_bounds(&self) -> Option<Bounds<Pixels>> {
        self.last_bounds
    }

    /// The field's words.
    pub fn text(&self) -> &str {
        &self.content
    }

    /// Says whether the owner's list of rows under the field shows (`open`) and has a row
    /// picked, so the field hands its arrow keys, Tab, Enter and Escape to it.
    pub fn set_mention_controls(
        &mut self,
        open: bool,
        has_selection: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mention_open == open && self.mention_has_selection == has_selection {
            return;
        }
        self.mention_open = open;
        self.mention_has_selection = has_selection;
        cx.notify();
    }

    /// Whether the field holds no words.
    pub const fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Sets the faint completion drawn after the words while the caret sits at their end.
    pub fn set_ghost(&mut self, ghost: Option<SharedString>, cx: &mut Context<Self>) {
        if self.ghost == ghost {
            return;
        }
        self.ghost = ghost;
        cx.notify();
    }

    /// Sets what the empty field shows.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// Replaces the words as a new document (a draft loaded, a field cleared): the caret goes
    /// to their end, and undo never reaches back past it.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = text.into();
        if self.single_line {
            self.content = self.content.replace(['\r', '\n'], " ");
        }
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.caret_affinity = CaretAffinity::Downstream;
        self.preferred_column = None;
        self.marked_range = None;
        self.scroll_top = 0.0;
        self.scroll_left = 0.0;
        self.follow_cursor = true;
        // Programmatic replacement (draft load, clear-on-submit) is a new
        // document, not an edit — undo must not reach back past it.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    // ---- undo history ----

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
            caret_affinity: self.caret_affinity,
        }
    }

    /// Called with the range about to be replaced, BEFORE the content changes,
    /// so the pushed snapshot is the pre-edit state.
    fn record_edit(&mut self, range: &Range<usize>, new_text: &str, now: Instant) {
        let kind = if new_text.is_empty() { EditKind::Delete } else { EditKind::Insert };
        // A run merges only while it stays single-character, contiguous with
        // the previous edit, of the same kind, and inside the idle window. A
        // pause, a word break, a paste, or a caret jump all break the run so
        // undo lands on a boundary the user recognizes.
        let coalescible = match kind {
            EditKind::Insert => range.is_empty() && new_text.chars().count() == 1,
            EditKind::Delete => self
                .content
                .get(range.clone())
                .is_some_and(|text| text.graphemes(true).count() == 1),
        };
        let mergeable = coalescible
            && match (kind, &self.last_edit) {
                (EditKind::Insert, Some((EditKind::Insert, at, when))) => {
                    range.is_empty()
                        && range.start == *at
                        && new_text.chars().count() == 1
                        && !new_text.starts_with(['\n', ' ', '\t'])
                        && now.saturating_duration_since(*when) < UNDO_COALESCE
                }
                (EditKind::Delete, Some((EditKind::Delete, at, when))) => {
                    range.end == *at && now.saturating_duration_since(*when) < UNDO_COALESCE
                }
                _ => false,
            };
        if !mergeable {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
        }
        // Any fresh edit invalidates the redo branch.
        self.redo_stack.clear();
        let tail = match kind {
            EditKind::Insert => range.start.saturating_add(new_text.len()),
            EditKind::Delete => range.start,
        };
        self.last_edit = coalescible.then_some((kind, tail, now));
    }

    fn restore(&mut self, snapshot: EditSnapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.caret_affinity = snapshot.caret_affinity;
        self.preferred_column = None;
        self.marked_range = None;
        self.follow_cursor = true;
        // Never merge a subsequent edit into a step that undo just crossed.
        self.last_edit = None;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(previous, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(next, cx);
    }

    // ---- editing ops ----

    /// The caret's byte offset: the selection's moving end.
    pub const fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.last_edit = None;
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.caret_affinity = CaretAffinity::Downstream;
        self.preferred_column = None;
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(ComposerInputEvent::CursorMoved);
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.last_edit = None;
        self.extend_selection(offset, cx);
    }

    // Deletion extends the selection internally without breaking a typing run.
    fn extend_selection(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.caret_affinity = CaretAffinity::Downstream;
        self.preferred_column = None;
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(ComposerInputEvent::CursorMoved);
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(ix, _)| (ix < offset).then_some(ix))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(ix, _)| (ix > offset).then_some(ix))
            .unwrap_or(self.content.len())
    }

    fn previous_word_boundary(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .rev()
            .find_map(|(ix, word)| (ix < offset && !word.trim().is_empty()).then_some(ix))
            .unwrap_or(0)
    }

    fn next_word_boundary(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .find_map(|(ix, word)| {
                let end = ix.saturating_add(word.len());
                (end > offset && !word.trim().is_empty()).then_some(end)
            })
            .unwrap_or(self.content.len())
    }

    /// Byte range of the logical line containing `offset`.
    fn line_range_at(&self, offset: usize) -> Range<usize> {
        let offset = self.content.floor_char_boundary(offset);
        let (before, after) = self.content.split_at(offset);
        let start = before.rfind('\n').map_or(0, |i| i.saturating_add(1));
        let end = after.find('\n').map_or(self.content.len(), |i| offset.saturating_add(i));
        start..end
    }

    /// Navigation stops before the whole line ending. The raw line range
    /// intentionally retains CR for full-line selection and block edits.
    fn line_content_end_at(&self, offset: usize) -> usize {
        let end = self.line_range_at(offset).end;
        let bytes = self.content.as_bytes();
        match end.checked_sub(1) {
            Some(cr) if bytes.get(end) == Some(&b'\n') && bytes.get(cr) == Some(&b'\r') => cr,
            _ => end,
        }
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.extend_selection(prev, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.extend_selection(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            self.move_to(prev, cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.selected_range.end);
            self.move_to(next, cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.mention_has_selection {
            cx.emit(ComposerInputEvent::MentionNavigate(-1));
            return;
        }
        if let Some((ix, affinity, column)) = self.vertical_target(-1.0) {
            self.move_to(ix, cx);
            self.caret_affinity = affinity;
            self.preferred_column = Some(column);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.mention_has_selection {
            cx.emit(ComposerInputEvent::MentionNavigate(1));
            return;
        }
        if let Some((ix, affinity, column)) = self.vertical_target(1.0) {
            self.move_to(ix, cx);
            self.caret_affinity = affinity;
            self.preferred_column = Some(column);
        }
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((ix, affinity, column)) = self.vertical_target(-1.0) {
            self.select_to(ix, cx);
            self.caret_affinity = affinity;
            self.preferred_column = Some(column);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((ix, affinity, column)) = self.vertical_target(1.0) {
            self.select_to(ix, cx);
            self.caret_affinity = affinity;
            self.preferred_column = Some(column);
        }
    }

    /// Offset one wrapped line above/below the cursor, keeping its x column.
    /// Clamps to the document edges, matching the platform's behavior on the
    /// first and last line.
    fn vertical_target(&self, dir: f32) -> Option<(usize, CaretAffinity, Pixels)> {
        let current = self.cursor_point()?;
        let column = self.preferred_column.unwrap_or(current.x);
        let target_y = dir.mul_add(f32::from(self.line_height), f32::from(current.y));
        if target_y < 0.0 {
            return Some((0, CaretAffinity::Downstream, column));
        }
        if target_y >= self.content_height {
            return Some((self.content.len(), CaretAffinity::Upstream, column));
        }
        let (index, affinity) = self.caret_for_point(point(column, px(target_y)));
        Some((index, affinity, column))
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_text(cx);
    }

    /// Select the whole draft — an inline rename opens with its name selected.
    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.move_to(line.start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_content_end_at(self.cursor_offset()), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.select_to(line.start, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_content_end_at(self.cursor_offset()), cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.move_to(prev, cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.move_to(next, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.select_to(prev, cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.select_to(next, cx);
    }

    /// Opt/Cmd + Delete family. With a live selection these delete the
    /// selection only (platform behavior) — the extend runs off the cursor.
    fn delete_to(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            if self.cursor_offset() == offset {
                return;
            }
            self.extend_selection(offset, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.delete_to(prev, window, cx);
    }

    fn delete_word_right(
        &mut self,
        _: &DeleteWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.delete_to(next, window, cx);
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = self.line_range_at(self.cursor_offset()).start;
        self.delete_to(start, window, cx);
    }

    fn delete_to_line_end(
        &mut self,
        _: &DeleteToLineEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let end = self.line_content_end_at(self.cursor_offset());
        self.delete_to(end, window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.content.get(self.selected_range.clone())
            && !text.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.content.get(self.selected_range.clone())
            && !text.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            self.last_edit = None;
            self.replace_text_in_range(None, "", window, cx);
            self.last_edit = None;
        }
    }

    fn paste_clipboard(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        // A field takes the clipboard's words; a picture or a copied file has none.
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // Clipboard operations remain separate undo steps, even a one-character paste.
            self.last_edit = None;
            self.replace_text_in_range(None, &text, window, cx);
            self.last_edit = None;
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        let line_end = self.line_range_at(self.cursor_offset()).end;
        let newline =
            if self.line_content_end_at(self.cursor_offset()) < line_end { "\r\n" } else { "\n" };
        self.replace_text_in_range(None, newline, window, cx);
    }

    /// Enter takes the host's highlighted row when it shows one, else submits.
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(if self.mention_has_selection {
            ComposerInputEvent::MentionAccept
        } else {
            ComposerInputEvent::Submitted
        });
    }

    /// Tab takes the host's highlighted row when it shows one, else moves on.
    fn mention_tab(&mut self, _: &MentionTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.mention_has_selection {
            cx.emit(ComposerInputEvent::MentionAccept);
        } else {
            cx.propagate();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if escape_dismisses_completion(&event.keystroke.key, self.mention_open) {
            cx.emit(ComposerInputEvent::MentionDismiss);
            cx.stop_propagation();
        }
    }

    // ---- geometry ----

    /// Content-local point for a byte index (y grows down from content top).
    fn point_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        self.point_for_index_with_affinity(index, CaretAffinity::Downstream)
    }

    fn cursor_point(&self) -> Option<Point<Pixels>> {
        self.point_for_index_with_affinity(self.cursor_offset(), self.caret_affinity)
    }

    /// Content-local point for a byte index, on the row `affinity` picks at a soft wrap.
    fn point_for_index_with_affinity(
        &self,
        index: usize,
        affinity: CaretAffinity,
    ) -> Option<Point<Pixels>> {
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let line_start = *self.line_starts.get(line_ix)?;
            let Some(relative_index) = index.checked_sub(line_start) else {
                continue;
            };
            if relative_index <= line.len() {
                let local = if affinity == CaretAffinity::Downstream {
                    line.wrap_boundaries()
                        .iter()
                        .position(|boundary| wrap_index(line, boundary) == Some(relative_index))
                        .map(|row| point(px(0.0), self.line_height * row.saturating_add(1)))
                        .or_else(|| line.position_for_index(relative_index, self.line_height))?
                } else {
                    line.position_for_index(relative_index, self.line_height)?
                };
                let y_offset: f32 = self
                    .last_lines
                    .iter()
                    .take(line_ix)
                    .map(|l| f32::from(l.size(self.line_height).height))
                    .sum();
                return Some(point(local.x, local.y + px(y_offset)));
            }
        }
        None
    }

    /// Byte index and visual side closest to a content-local point.
    fn caret_for_point(&self, position: Point<Pixels>) -> (usize, CaretAffinity) {
        if self.display_is_placeholder {
            return (0, CaretAffinity::Downstream);
        }
        let mut y = f32::from(position.y);
        if y < 0.0 {
            return (0, CaretAffinity::Downstream);
        }
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let height = f32::from(line.size(self.line_height).height);
            let line_start = self.line_starts.get(line_ix).copied().unwrap_or(0);
            if y < height || line_ix.saturating_add(1) == self.last_lines.len() {
                let local = point(position.x, px(y.min(height - 1.0).max(0.0)));
                let ix = line
                    .closest_index_for_position(local, self.line_height)
                    .unwrap_or_else(|ix| ix);
                let affinity = line
                    .wrap_boundaries()
                    .iter()
                    .position(|boundary| wrap_index(line, boundary) == Some(ix))
                    .filter(|row| local.y < self.line_height * row.saturating_add(1))
                    .map_or(CaretAffinity::Downstream, |_| CaretAffinity::Upstream);
                let mut raw = line_start.saturating_add(ix).min(self.content.len());
                // CRLF is one newline grapheme. A click past the text on its
                // row must land before CR, never between its two bytes.
                if let Some(cr) = raw.checked_sub(1)
                    && self.content.as_bytes().get(raw) == Some(&b'\n')
                    && self.content.as_bytes().get(cr) == Some(&b'\r')
                {
                    raw = cr;
                }
                return (raw, affinity);
            }
            y -= height;
        }
        (self.content.len(), CaretAffinity::Upstream)
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        self.caret_for_mouse_position(position).0
    }

    fn caret_for_mouse_position(&self, position: Point<Pixels>) -> (usize, CaretAffinity) {
        let Some(bounds) = self.last_bounds else {
            return (0, CaretAffinity::Downstream);
        };
        let local = point(
            position.x - bounds.left() + px(self.scroll_left),
            position.y - bounds.top() + px(self.scroll_top),
        );
        self.caret_for_point(local)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let intent = press_intent(event.click_count, event.modifiers.shift);
        self.is_selecting = true;
        self.drag_position = Some(event.position);
        self.drag_unit = None;
        self.drag_generation = self.drag_generation.wrapping_add(1);
        self.drag_autoscroll_active = false;
        match intent {
            PressIntent::Word | PressIntent::Line => {
                let index = self.index_for_mouse_position(event.position);
                let range = self.selection_unit(intent, index);
                self.move_to(range.start, cx);
                self.select_to(range.end, cx);
                self.drag_unit = Some((intent, range));
            }
            PressIntent::ExtendSelection => {
                let (index, affinity) = self.caret_for_mouse_position(event.position);
                self.select_to(index, cx);
                self.caret_affinity = affinity;
            }
            PressIntent::PlaceCaret => {
                let (index, affinity) = self.caret_for_mouse_position(event.position);
                self.move_to(index, cx);
                self.caret_affinity = affinity;
            }
        }
    }

    fn selection_unit(&self, intent: PressIntent, index: usize) -> Range<usize> {
        if intent == PressIntent::Line {
            let mut range = self.line_range_at(index);
            if range.end < self.content.len() {
                range.end = range.end.saturating_add(1);
            }
            range
        } else {
            word_range(&self.content, index)
        }
    }

    fn drag_select_to(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some((intent, anchor)) = self.drag_unit.clone() {
            let range = self.selection_unit(intent, index);
            if range.start < anchor.start {
                self.move_to(anchor.end, cx);
                self.select_to(range.start, cx);
            } else {
                self.move_to(anchor.start, cx);
                self.select_to(range.end.max(anchor.end), cx);
            }
        } else {
            self.select_to(index, cx);
        }
    }

    fn drag_select_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (index, affinity) = self.caret_for_mouse_position(position);
        self.drag_select_to(index, cx);
        if self.drag_unit.is_none() {
            self.caret_affinity = affinity;
        }
    }

    const fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.drag_position = None;
        self.drag_generation = self.drag_generation.wrapping_add(1);
        self.drag_autoscroll_active = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.drag_position = Some(event.position);
            let position = self.drag_selection_position(event.position);
            self.drag_select_at(position, cx);
            if self.drag_scroll_delta(event.position) != 0.0 && !self.drag_autoscroll_active {
                self.start_drag_autoscroll(cx);
            }
        }
    }

    fn start_drag_autoscroll(&mut self, cx: &Context<Self>) {
        self.drag_autoscroll_active = true;
        let generation = self.drag_generation;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(DRAG_SCROLL_FRAME_MS)).await;
                let keep_running = this
                    .update(cx, |input, cx| input.drag_autoscroll_tick(generation, cx))
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn drag_selection_position(&self, position: Point<Pixels>) -> Point<Pixels> {
        let Some(bounds) = self.last_bounds else {
            return position;
        };
        point(
            position.x.clamp(bounds.left(), bounds.right() - px(0.5)),
            position.y.clamp(bounds.top(), bounds.bottom() - px(0.5)),
        )
    }

    fn drag_scroll_delta(&self, position: Point<Pixels>) -> f32 {
        let Some(bounds) = self.last_bounds else {
            return 0.0;
        };
        input_drag_scroll_delta(
            f32::from(position.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
            f32::from(self.line_height),
        )
    }

    fn drag_autoscroll_tick(&mut self, generation: u64, cx: &mut Context<Self>) -> bool {
        if !self.is_selecting || self.drag_generation != generation {
            return false;
        }
        let (Some(position), Some(bounds)) = (self.drag_position, self.last_bounds) else {
            self.drag_autoscroll_active = false;
            return false;
        };
        let delta = self.drag_scroll_delta(position);
        if delta == 0.0 {
            self.drag_autoscroll_active = false;
            return false;
        }
        let next = (self.scroll_top + delta).clamp(
            0.0,
            input_max_scroll(
                self.content_height,
                self.settled_viewport_height.unwrap_or_else(|| f32::from(bounds.size.height)),
            ),
        );
        if next.to_bits() == self.scroll_top.to_bits() {
            self.drag_autoscroll_active = false;
            return false;
        }
        self.scroll_top = next;
        let edge_position = self.drag_selection_position(position);
        self.drag_select_at(edge_position, cx);
        // Selection motion normally resumes caret following. During an edge
        // drag the autoscroll loop owns the viewport instead.
        self.follow_cursor = false;
        true
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let viewport_height =
            self.settled_viewport_height.unwrap_or_else(|| f32::from(bounds.size.height));
        let delta_y = f32::from(event.delta.pixel_delta(self.line_height).y);
        let next =
            input_scroll_offset(self.scroll_top, delta_y, self.content_height, viewport_height);
        if next.to_bits() == self.scroll_top.to_bits() {
            // Overscroll guard: when the input itself is scrollable (content
            // taller than the viewport), swallow the wheel event even at the
            // scroll boundary so it never chains into the outer transcript
            // list (the native equivalent of `overscroll-behavior: contain`).
            if delta_y != 0.0 && input_max_scroll(self.content_height, viewport_height) > 0.0 {
                cx.stop_propagation();
            }
            return;
        }
        self.scroll_top = next;
        self.follow_cursor = false;
        cx.stop_propagation();
        cx.emit(ComposerInputEvent::ViewportChanged);
        cx.notify();
    }

    // ---- utf16 mapping (IME) ----

    fn offset_from_utf16(&self, offset: usize) -> usize {
        Self::utf8_offset(&self.content, offset)
    }

    fn utf8_offset(text: &str, offset: usize) -> usize {
        let mut utf8_offset = 0_usize;
        let mut utf16_count = 0_usize;
        for ch in text.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count = utf16_count.saturating_add(ch.len_utf16());
            utf8_offset = utf8_offset.saturating_add(ch.len_utf8());
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0_usize;
        let mut utf8_count = 0_usize;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count = utf8_count.saturating_add(ch.len_utf8());
            utf16_offset = utf16_offset.saturating_add(ch.len_utf16());
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// Shape the text at a width; store measured layout; return content height.
    /// Called from the element's measured-layout closure.
    fn layout_text(&mut self, width: Pixels, style: &TextStyle, window: &Window) -> f32 {
        let key = InputLayoutKey {
            width,
            font: style.font(),
            font_size: style.font_size.to_pixels(window.rem_size()),
            color: style.color,
            marked_range: self.marked_range.clone(),
            placeholder: self.placeholder.clone(),
        };
        // Height-only animation, scrolling, selection and caret blinking do
        // not change shaping. Reuse the entity's single retained layout,
        // including the parent's early measurement of this same edit.
        if !self.needs_measure && self.last_layout_key.as_ref() == Some(&key) {
            self.layout_epoch = self.layout_epoch.wrapping_add(1);
            return self.content_height;
        }
        #[cfg(test)]
        {
            self.layout_rebuilds = self.layout_rebuilds.saturating_add(1);
        }
        let (display, is_placeholder) = if self.content.is_empty() {
            (self.placeholder.clone(), true)
        } else {
            (SharedString::from(self.content.clone()), false)
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        self.line_height = px(self.configured_line_height);

        let run_for = |len: usize, underline: bool| TextRun {
            len,
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: underline.then_some(UnderlineStyle {
                color: Some(style.color),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: None,
        };
        let marked = self.marked_range.clone().filter(|_| !is_placeholder);
        let mut boundaries = vec![0, display.len()];
        if let Some(range) = &marked {
            boundaries.extend([range.start, range.end]);
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let runs: Vec<TextRun> = boundaries
            .iter()
            .zip(boundaries.iter().skip(1))
            .filter_map(|(&start, &end)| {
                let len = end.checked_sub(start).filter(|len| *len > 0)?;
                Some(run_for(len, marked.as_ref().is_some_and(|range| range.contains(&start))))
            })
            .collect();
        #[cfg(test)]
        {
            self.last_runs = runs.clone();
        }

        let mut lines = Vec::new();
        let mut display_at = 0_usize;
        let mut run_at = 0_usize;
        let run_ranges: Vec<_> = runs
            .iter()
            .map(|run| {
                let range = run_at..run_at.saturating_add(run.len);
                run_at = range.end;
                (range, run)
            })
            .collect();
        for text in display.split('\n') {
            let end = display_at.saturating_add(text.len());
            let first_run = run_ranges.partition_point(|(r, _)| r.end <= display_at);
            let line_runs: Vec<TextRun> = run_ranges
                .get(first_run..)
                .unwrap_or_default()
                .iter()
                .take_while(|(r, _)| r.start < end)
                .filter_map(|(range, run)| {
                    let len = range.end.min(end).saturating_sub(range.start.max(display_at));
                    (len > 0).then(|| TextRun { len, ..(*run).clone() })
                })
                .collect();
            if let Ok(shaped) = window.text_system().shape_text(
                text.to_owned().into(),
                font_size,
                &line_runs,
                (!self.single_line).then_some(width.max(px(20.0))),
                None,
            ) {
                lines.extend(shaped);
            }
            display_at = end.saturating_add(1);
        }

        // Logical line byte offsets (each shaped line covers one \n-split line).
        let mut line_starts = Vec::with_capacity(lines.len());
        let mut at = 0_usize;
        for line in &lines {
            line_starts.push(at);
            at = at.saturating_add(line.len()).saturating_add(1); // + '\n'
        }
        if line_starts.is_empty() {
            line_starts.push(0);
        }

        let content_height: f32 =
            lines.iter().map(|l| f32::from(l.size(self.line_height).height)).sum();
        let max_line_width: f32 =
            lines.iter().map(|l| f32::from(l.unwrapped_layout.width)).fold(0.0, f32::max);

        self.display_is_placeholder = is_placeholder;
        self.max_ascent = lines
            .iter()
            .map(|line| f32::from(line.unwrapped_layout.ascent))
            .fold(INPUT_TEXT_SIZE, f32::max);
        self.last_layout_key = Some(key);
        self.last_lines = lines;
        self.line_starts = line_starts;
        self.content_height = content_height.max(self.configured_line_height);
        self.max_line_width = if is_placeholder { 0.0 } else { max_line_width };
        self.last_width = f32::from(width);
        self.needs_measure = false;
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.content_height
    }

    fn paint_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        let visible = f32::from(bounds.size.height);
        let top_overflow = input_overflow_edges(
            self.content_height,
            self.settled_viewport_height.unwrap_or(visible),
            visible,
            self.scroll_top,
        )
        .0;
        let top_padding = if top_overflow { self.overflow_top_padding } else { 0.0 };
        let height = input_reveal_height(
            visible,
            self.scroll_top,
            f32::from(self.line_height),
            self.resizing,
        );
        Bounds::new(
            point(bounds.left(), bounds.top() - px(top_padding)),
            size(bounds.size.width, px(height + top_padding)),
        )
    }

    /// Keep the cursor visible when content exceeds the element height.
    fn clamp_scroll(&mut self, element_height: f32) -> bool {
        if self.single_line {
            let previous = self.scroll_left;
            let width = (self.last_width - 2.0).max(1.0);
            if let Some(cursor) = self.cursor_point() {
                let x = f32::from(cursor.x);
                self.scroll_left = self.scroll_left.min(x).max(x - width).max(0.0);
            }
            self.scroll_left = self.scroll_left.min((self.max_line_width - width).max(0.0));
            self.scroll_top = 0.0;
            return self.scroll_left.to_bits() != previous.to_bits();
        }
        let previous = self.scroll_top;
        if self.follow_cursor
            && let Some(cursor) = self.cursor_point()
        {
            self.scroll_top = input_scroll_offset_for_cursor(
                self.scroll_top,
                f32::from(cursor.y),
                f32::from(self.line_height),
                self.content_height,
                element_height,
                self.settled_viewport_height,
            );
        }
        self.scroll_top = self.scroll_top.clamp(
            0.0,
            input_max_scroll(
                self.content_height,
                self.settled_viewport_height.unwrap_or(element_height),
            ),
        );
        self.scroll_top.to_bits() != previous.to_bits()
    }
}

impl EventEmitter<ComposerInputEvent> for ComposerInput {}

impl std::fmt::Debug for ComposerInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposerInput")
            .field("key_context", &self.key_context)
            .field("len", &self.content.len())
            .field("selected_range", &self.selected_range)
            .finish_non_exhaustive()
    }
}

impl Focusable for ComposerInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for ComposerInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        adjusted_range.replace(self.range_to_utf16(&range));
        Some(self.content.get(range)?.to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range.take().is_some() {
            self.needs_measure = true;
            self.last_edit = None;
            cx.emit(ComposerInputEvent::CursorMoved);
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let single_line_text;
        let new_text = if self.single_line {
            single_line_text = text.replace(['\r', '\n'], " ");
            single_line_text.as_str()
        } else {
            text
        };
        let range = range
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        // An IME commit is the tail of a composition whose pre-composition
        // snapshot was already taken (`replace_and_mark_text_in_range`);
        // recording here would pin undo to the half-composed text instead.
        if self.marked_range.is_none() {
            self.record_edit(&range, new_text, crate::clock::now(cx));
        }
        self.content.replace_range(range.clone(), new_text);
        let cursor = range.start.saturating_add(new_text.len());
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.caret_affinity = CaretAffinity::Downstream;
        self.preferred_column = None;
        self.marked_range.take();
        self.follow_cursor = true;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let single_line_text;
        let new_text = if self.single_line {
            single_line_text = new_text.replace(['\r', '\n'], " ");
            single_line_text.as_str()
        } else {
            new_text
        };
        let range = range
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        // First keystroke of a composition: snapshot the text as it stood
        // before any of it existed, so one undo drops the whole composition.
        if self.marked_range.is_none() {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
            self.last_edit = None;
        }
        self.content.replace_range(range.clone(), new_text);
        let end = range.start.saturating_add(new_text.len());
        self.marked_range = (!new_text.is_empty()).then_some(range.start..end);
        self.selected_range = new_selected_range
            .as_ref()
            .map(|r| Self::utf8_offset(new_text, r.start)..Self::utf8_offset(new_text, r.end))
            .map_or(end..end, |new_range| {
                new_range.start.saturating_add(range.start)
                    ..new_range.end.saturating_add(range.start)
            });
        self.selection_reversed = false;
        self.caret_affinity = CaretAffinity::Downstream;
        self.preferred_column = None;
        self.follow_cursor = true;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let start = if range.is_empty() && range.start == self.cursor_offset() {
            self.cursor_point()?
        } else {
            self.point_for_index(range.start)?
        };
        let origin = point(
            element_bounds.left() + start.x - px(self.scroll_left),
            element_bounds.top() + start.y - px(self.scroll_top),
        );
        Some(Bounds::new(origin, size(px(2.0), self.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_for_mouse_position(point);
        Some(self.offset_to_utf16(index))
    }
}

/// The custom element: measured auto-grow layout + shaped-line painting.
struct ComposerTextElement {
    input: Entity<ComposerInput>,
    /// Max content height before internal scrolling kicks in.
    max_content_height: f32,
}

struct ComposerTextPrepaint {
    cursor: Option<PaintQuad>,
    selection_quads: Vec<PaintQuad>,
    /// Completion preview: window-space origin of the end-of-text caret plus
    /// the suffix to paint there (shaped at paint time — it never joins the
    /// content's own layout, so hit-testing and the caret ignore it).
    ghost: Option<(Point<Pixels>, SharedString)>,
}

impl IntoElement for ComposerTextElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for ComposerTextElement {
    type PrepaintState = ComposerTextPrepaint;
    type RequestLayoutState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let input = self.input.clone();
        let text_style = window.text_style();
        let max_content = self.max_content_height;
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, cx| {
                let width = known.width.unwrap_or(match available.width {
                    gpui::AvailableSpace::Definite(width) => width,
                    _ => px(320.0),
                });
                let content_height =
                    input.update(cx, |input, _| input.layout_text(width, &text_style, window));
                size(width, px(content_height.min(max_content)))
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let text_style = window.text_style();
        self.input.update(cx, |input, cx| {
            // Intrinsic measurement may try several widths in one layout.
            // Only publish the resolved geometry, once: notifying for each
            // provisional width starts an endless measure/notify loop.
            input.layout_text(bounds.size.width, &text_style, window);
            let layout = (bounds.size.width, input.content_height);
            let layout_changed = input.last_notified_layout != Some(layout);
            input.last_notified_layout = Some(layout);
            let scrolled = input.clamp_scroll(f32::from(bounds.size.height));
            input.last_bounds = Some(bounds);
            if scrolled || layout_changed {
                cx.emit(ComposerInputEvent::ViewportChanged);
            }
        });
        let input = self.input.read(cx);
        let scroll = px(input.scroll_top);
        let origin = point(bounds.left() - px(input.scroll_left), bounds.top() - scroll);
        let selection_color = Theme::of(cx).selection;
        let caret_color = Theme::of(cx).caret;

        let mut selection_quads = Vec::new();
        let mut cursor = None;
        if input.selected_range.is_empty() || input.display_is_placeholder {
            if let Some(p) = input.cursor_point() {
                cursor = Some(fill(
                    Bounds::new(
                        point(origin.x + p.x, origin.y + p.y),
                        size(px(2.0), input.line_height),
                    ),
                    caret_color,
                ));
            } else if input.display_is_placeholder {
                cursor =
                    Some(fill(Bounds::new(origin, size(px(2.0), input.line_height)), caret_color));
            }
        } else if let (Some(start), Some(end)) = (
            input.point_for_index(input.selected_range.start),
            input.point_for_index_with_affinity(input.selected_range.end, CaretAffinity::Upstream),
        ) {
            let lh = input.line_height;
            if start.y == end.y {
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(origin.x + end.x, origin.y + start.y + lh),
                    ),
                    selection_color,
                ));
            } else {
                // First visual row, full middle rows, last visual row.
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(bounds.right(), origin.y + start.y + lh),
                    ),
                    selection_color,
                ));
                if end.y > start.y + lh {
                    selection_quads.push(fill(
                        Bounds::from_corners(
                            point(origin.x, origin.y + start.y + lh),
                            point(bounds.right(), origin.y + end.y),
                        ),
                        selection_color,
                    ));
                }
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x, origin.y + end.y),
                        point(origin.x + end.x, origin.y + end.y + lh),
                    ),
                    selection_color,
                ));
            }
        }
        // The ghost only shows where accepting it would insert: a collapsed
        // caret at the end of real (non-placeholder, non-IME) text.
        let ghost = input
            .ghost
            .clone()
            .filter(|g| {
                !g.is_empty()
                    && !input.display_is_placeholder
                    && input.marked_range.is_none()
                    && input.selected_range.is_empty()
                    && input.cursor_offset() == input.content.len()
            })
            .and_then(|g| {
                input
                    .point_for_index(input.content.len())
                    .map(|p| (point(origin.x + p.x, origin.y + p.y), g))
            });
        ComposerTextPrepaint { cursor, selection_quads, ghost }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |input, cx| input.on_mouse_move(event, cx));
            }
        });

        // WrappedLine isn't Clone — temporarily take the shaped lines out of the
        // entity for painting, then put them back for mouse mapping.
        let (lines, line_height, scroll, scroll_left) = self.input.update(cx, |input, _| {
            (
                std::mem::take(&mut input.last_lines),
                input.line_height,
                input.scroll_top,
                input.scroll_left,
            )
        });

        let paint_bounds = self.input.read(cx).paint_bounds(bounds);
        window.with_content_mask(Some(gpui::ContentMask { bounds: paint_bounds }), |window| {
            for quad in prepaint.selection_quads.drain(..) {
                window.paint_quad(quad);
            }
            let mut y = bounds.top() - px(scroll);
            for line in &lines {
                let height = line.size(line_height).height;
                if let Err(error) = line.paint(
                    point(bounds.left() - px(scroll_left), y),
                    line_height,
                    gpui::TextAlign::Left,
                    Some(bounds),
                    window,
                    cx,
                ) {
                    tracing::debug!(%error, "paint field text");
                }
                y += height;
            }
            if let Some((ghost_origin, ghost)) = prepaint.ghost.take() {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let run = TextRun {
                    len: ghost.len(),
                    font: style.font(),
                    color: Theme::of(cx).text_faint,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(ghost, font_size, &[run], None);
                // (Clipping comes from the surrounding content mask.)
                if let Err(error) =
                    line.paint(ghost_origin, line_height, gpui::TextAlign::Left, None, window, cx)
                {
                    tracing::debug!(%error, "paint field ghost");
                }
            }
            // Caret only when this input is actually focused in an active
            // window (Electron hides it on window deactivation too), and only
            // in the "on" blink phase — solid while typing, ~500ms blink idle.
            if self.input.update(cx, |input, cx| input.caret_shown(window, cx))
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        self.input.update(cx, |input, _| {
            input.last_lines = lines;
        });
    }
}

impl Render for ComposerInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let popup_theme = theme.for_popup();
        let theme = if self.key_context == "PaletteSearch"
            || self.accessibility_role == Role::SearchInput
        {
            &popup_theme
        } else {
            theme
        };
        let text_color = if self.content.is_empty() { theme.text_faint } else { theme.text };
        div()
            .id(("composer-input", cx.entity_id()))
            .role(self.accessibility_role)
            .aria_label(self.name.clone().unwrap_or_else(|| self.placeholder.clone()))
            .aria_placeholder(self.placeholder.clone())
            // A screen reader reads the words in the field, not only its placeholder.
            .when(!self.content.is_empty(), |el| el.aria_value(self.content.clone()))
            .key_context(self.key_context)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::mention_tab))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                // Capture runs before the clicked control handles the press, so
                // another input can take focus normally during bubbling.
                if event.button == MouseButton::Left && this.focus_handle.is_focused(window) {
                    window.blur(cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_scroll))
            .w_full()
            .text_size(crate::typography::ui_rems(self.text_size))
            .line_height(crate::typography::ui_rems(self.configured_line_height))
            .text_color(text_color)
            .font_family(theme.font_sans.clone())
            .child({
                let input = cx.entity();
                let ascent = self.max_ascent;
                crate::edge_fade::edge_faded(
                    INPUT_FADE_BAND,
                    true,
                    true,
                    ComposerTextElement {
                        input: input.clone(),
                        max_content_height: self.viewport_height.unwrap_or(INPUT_MAX_HEIGHT),
                    },
                )
                // Fade through the existing top padding, like the transcript
                // scrolling under its chrome. Account for GPUI's baseline
                // sampling without consuming another inset inside the text box.
                .inset_top(ascent - self.overflow_top_padding)
                .fade_overflow_y_with(move |cx| {
                    let input = input.read(cx);
                    let visible_height =
                        input.last_bounds.map_or(0.0, |bounds| f32::from(bounds.size.height));
                    input_overflow_edges(
                        input.content_height,
                        input.settled_viewport_height.unwrap_or(INPUT_MAX_HEIGHT),
                        visible_height,
                        input.scroll_top,
                    )
                })
            })
    }
}

/// The press of a control drawn `width` by `height` pt: on a touch screen it takes a
/// finger's 44 pt around what it draws, its negative margins giving the room back, so the layout,
/// the drawn wash and the focus ring (`focus_ring_within` with the reach) stay put (#23). Under a
/// pointer it is the drawn size. On a touch screen its press raises no keyboard: the composer's
/// pill leaves the field unfocused for it. Returns the press and how far it reaches past each edge.
pub(crate) fn finger_press(
    id: impl Into<gpui::ElementId>,
    width: f32,
    height: f32,
) -> (gpui::Stateful<gpui::Div>, f32) {
    let (x, y) =
        (crate::surface_chrome::touch_reach(width), crate::surface_chrome::touch_reach(height));
    let press = div()
        .id(id)
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            if crate::shell::touch_screen() {
                window.prevent_default();
            }
        })
        .flex_none()
        .w(px(x.mul_add(2.0, width)))
        .h(px(y.mul_add(2.0, height)))
        .mx(px(-x))
        .my(px(-y))
        .flex()
        .items_center()
        .justify_center();
    (press, x.min(y))
}

/// The colour of one of xterm's 256 colours, the first 16 from the terminal theme: a
/// code fence's terminal colours (`markdown/ansi.rs`).
pub(crate) fn ansi_color(index: u8, theme: &Theme) -> gpui::Hsla {
    const LEVELS: [u32; 6] = [0, 95, 135, 175, 215, 255];
    let level = |n: u8| LEVELS.get(usize::from(n)).copied().unwrap_or_default();
    let rgb = match index {
        0..=15 => {
            return theme
                .terminal
                .ansi
                .get(usize::from(index))
                .copied()
                .unwrap_or_else(gpui::transparent_black);
        }
        16..=231 => {
            let n = index.saturating_sub(16);
            (level(n / 36) << 16) | (level(n / 6 % 6) << 8) | level(n % 6)
        }
        _ => {
            let grey = u32::from(index.saturating_sub(232)).saturating_mul(10).saturating_add(8);
            (grey << 16) | (grey << 8) | grey
        }
    };
    gpui::rgb(rgb).into()
}

#[cfg(test)]
mod modal_selection_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A field alone in a window.
    struct InputHarness {
        input: Entity<ComposerInput>,
    }

    impl Render for InputHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.input.clone())
        }
    }

    fn composer_focus_window(
        cx: &mut gpui::TestAppContext,
    ) -> (tempfile::TempDir, gpui::WindowHandle<InputHarness>) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::app_menus::init(cx);
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let input = cx.new(|cx| ComposerInput::new("Do anything…", cx));
            InputHarness { input }
        });
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        (dir, window)
    }

    /// Under Reduce Motion the caret of a focused composer in an active window stays on, with
    /// no driver flipping it; otherwise it is off a half-period on.
    #[gpui::test]
    fn reduce_motion_holds_the_caret_still(cx: &mut gpui::TestAppContext) {
        let (_dir, handle) = composer_focus_window(cx);
        let input = handle.update(cx, |composer, _, _| composer.input.clone()).unwrap();
        handle
            .update(cx, |_, window, cx| {
                window.activate_window();
                window.focus(&input.read(cx).focus_handle(cx), cx);
            })
            .unwrap();
        let caret = |reduced: bool, cx: &mut gpui::TestAppContext| {
            cx.update(|cx| crate::motion::set_reduced_motion(cx, reduced));
            let shown = |cx: &mut gpui::TestAppContext| {
                handle
                    .update(cx, |_, window, cx| {
                        input.update(cx, |input, cx| {
                            (input.caret_shown(window, cx), input.blink_task.is_some())
                        })
                    })
                    .unwrap()
            };
            input.update(cx, |input, _| input.reset_blink());
            let _armed = shown(cx);
            cx.executor().advance_clock(Duration::from_millis(CARET_BLINK_MS + 1));
            cx.run_until_parked();
            shown(cx)
        };
        assert_eq!(caret(false, cx), (false, true), "a half-period on, off");
        assert_eq!(caret(true, cx), (true, false), "held still, nothing driving it");
    }

    fn with_composer_input(
        cx: &mut gpui::TestAppContext,
        test: impl FnOnce(&mut ComposerInput, &mut Window, &mut Context<ComposerInput>),
    ) {
        let (_dir, handle) = composer_focus_window(cx);
        handle
            .update(cx, |composer, window, cx| {
                composer.input.update(cx, |input, cx| test(input, window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    fn inputs_release_focus_on_click_away(cx: &mut gpui::TestAppContext) {
        struct Inputs(Vec<Entity<ComposerInput>>);
        impl Render for Inputs {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(self.0.iter().map(|input| div().w(px(200.0)).child(input.clone())))
            }
        }
        cx.update(|cx| cx.set_global(Theme::dark()));
        let host = cx.add_window(|_, cx| {
            Inputs(vec![
                cx.new(|cx| ComposerInput::new("Composer", cx)),
                cx.new(|cx| ComposerInput::new("URL", cx).with_single_line()),
            ])
        });
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        let inputs = host.read_with(cx, |host, _| host.0.clone()).unwrap();
        // Visit both fields and return to the first: click-away capture must
        // never clear focus acquired by the clicked input during bubbling.
        for index in [0, 1, 0] {
            let position =
                inputs[index].read_with(cx, |input, _| input.last_bounds.unwrap().center());
            cx.update_window(host.into(), |_, window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        click_count: 1,
                        ..Default::default()
                    }),
                    cx,
                );
                assert!(inputs[index].read(cx).focus_handle.is_focused(window));
                window.dispatch_event(
                    gpui::PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        ..Default::default()
                    }),
                    cx,
                );
            })
            .unwrap();
        }
        cx.update_window(host.into(), |_, window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: point(px(400.0), px(300.0)),
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            assert!(window.focused(cx).is_none());
        })
        .unwrap();
    }

    #[gpui::test]
    fn shift_backspace_deletes_text_and_selection_in_all_input_contexts(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            init(cx);
        });
        for context in [GENERIC_COMPOSER_CONTEXT, PALETTE_SEARCH_CONTEXT] {
            let handle = cx.add_window(|window, cx| {
                let mut input = ComposerInput::with_context("", context, cx);
                input.set_text("café", cx);
                window.focus(&input.focus_handle, cx);
                input
            });
            cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx)).unwrap();
            cx.simulate_keystrokes(handle.into(), "shift-backspace");
            handle
                .update(cx, |input, _, cx| {
                    assert_eq!(input.text(), "caf", "{context}");
                    input.set_text("hello world", cx);
                    input.move_to(6, cx);
                    input.extend_selection(11, cx);
                })
                .unwrap();
            cx.simulate_keystrokes(handle.into(), "shift-backspace");
            handle
                .update(cx, |input, _, cx| {
                    assert_eq!(input.text(), "hello ", "{context}");
                    input.set_text("", cx);
                })
                .unwrap();
            cx.simulate_keystrokes(handle.into(), "shift-backspace");
            assert_eq!(handle.read_with(cx, |input, _| input.text().to_owned()).unwrap(), "");
        }
    }

    #[gpui::test]
    fn crlf_navigation_deletion_and_newlines_keep_the_pair_intact(cx: &mut gpui::TestAppContext) {
        with_composer_input(cx, |input, window, cx| {
            // A plain field: a list item's newline is a newline, no marker carried on.
            for first in ["plain café", "- item", "- [x] item"] {
                let raw = format!("{first}\r\nnext");
                let next_start = format!("{first}\r\n").len();
                input.set_text(&raw, cx);
                input.move_to(0, cx);
                input.end(&End, window, cx);
                assert_eq!(input.cursor_offset(), first.len());
                input.layout_text(px(600.0), &window.text_style(), window);
                assert_eq!(input.caret_for_point(point(px(590.0), px(1.0))).0, first.len());
                assert_eq!(input.selection_unit(PressIntent::Line, 0), 0..next_start);
                input.move_to(0, cx);
                input.select_end(&SelectEnd, window, cx);
                assert_eq!(input.selected_range, 0..first.len());
                input.move_to(0, cx);
                input.end(&End, window, cx);
                input.backspace(&Backspace, window, cx);
                let previous = first.grapheme_indices(true).next_back().unwrap().0;
                assert_eq!(input.text(), format!("{}\r\nnext", first.split_at(previous).0));
                input.undo(&Undo, window, cx);
                input.move_to(first.len(), cx);
                input.delete(&Delete, window, cx);
                assert_eq!(input.text(), format!("{first}next"));
                input.undo(&Undo, window, cx);
                input.move_to(next_start, cx);
                input.backspace(&Backspace, window, cx);
                assert_eq!(input.text(), format!("{first}next"));
                input.undo(&Undo, window, cx);
                input.move_to(0, cx);
                input.end(&End, window, cx);
                input.newline(&Newline, window, cx);
                assert_eq!(input.text(), format!("{first}\r\n\r\nnext"));
                input.undo(&Undo, window, cx);
                input.move_to(2, cx);
                input.delete_to_line_end(&DeleteToLineEnd, window, cx);
                assert_eq!(input.text(), format!("{}\r\nnext", first.split_at(2).0));
                input.undo(&Undo, window, cx);
                assert_eq!(input.text(), raw);
            }
            input.set_text("\r\nnext", cx);
            input.move_to(0, cx);
            input.end(&End, window, cx);
            assert_eq!(input.cursor_offset(), 0);
            input.backspace(&Backspace, window, cx);
            assert_eq!(input.text(), "\r\nnext");
            input.delete(&Delete, window, cx);
            assert_eq!(input.text(), "next");
        });
    }

    #[gpui::test]
    fn soft_wrap_carets_preserve_pointer_keyboard_and_ime_affinity(cx: &mut gpui::TestAppContext) {
        let (_dir, handle) = composer_focus_window(cx);
        handle.update(cx, |composer, window, cx| {
            composer.input.update(cx, |input, cx| {
                input.set_text("Café and emoji 🦀 wrap across several visual rows with enough text to keep moving downward.", cx);
                input.layout_text(px(140.0), &window.text_style(), window);
                let line = &input.last_lines[0];
                assert!(line.wrap_boundaries().len() >= 2);
                let boundary = line.wrap_boundaries()[0];
                let offset = line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index;
                let upstream = input.point_for_index_with_affinity(offset, CaretAffinity::Upstream).unwrap();
                let downstream = input.point_for_index(offset).unwrap();
                assert_eq!(upstream.y, px(0.0));
                assert_eq!(downstream, point(px(0.0), input.line_height));
                assert!(upstream.x > downstream.x);
                assert_eq!(input.caret_for_point(upstream + point(px(20.0), px(1.0))), (offset, CaretAffinity::Upstream));
                assert_eq!(input.caret_for_point(downstream + point(px(0.0), px(1.0))), (offset, CaretAffinity::Downstream));
                let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(140.0), px(200.0)));
                input.last_bounds = Some(bounds);
                for (local, affinity) in [(upstream, CaretAffinity::Upstream), (downstream, CaretAffinity::Downstream)] {
                    input.on_mouse_down(&MouseDownEvent {
                        button: MouseButton::Left,
                        position: bounds.origin + local + point(px(0.0), px(1.0)),
                        click_count: 1,
                        ..Default::default()
                    }, window, cx);
                    assert_eq!(input.cursor_offset(), offset);
                    assert_eq!(input.caret_affinity, affinity);
                    assert_eq!(input.cursor_point(), Some(local));
                    let utf16 = input.offset_to_utf16(offset);
                    let ime = input.bounds_for_range(utf16..utf16, bounds, window, cx).unwrap();
                    assert_eq!(ime.origin, bounds.origin + local);
                    input.replace_text_in_range(None, "x", window, cx);
                    input.undo(&Undo, window, cx);
                    input.layout_text(px(140.0), &window.text_style(), window);
                    assert_eq!(input.cursor_offset(), offset);
                    assert_eq!(input.caret_affinity, affinity);
                    assert_eq!(input.cursor_point(), Some(local));
                }
                input.move_to(0, cx);
                input.down(&Down, window, cx);
                assert_eq!(input.cursor_point(), Some(downstream));
                input.down(&Down, window, cx);
                assert_eq!(input.cursor_point().unwrap().y, input.line_height * 2);
                input.up(&Up, window, cx);
                assert_eq!(input.cursor_point(), Some(downstream));
                input.up(&Up, window, cx);
                assert_eq!(input.cursor_offset(), 0);
                input.select_down(&SelectDown, window, cx);
                assert_eq!(input.selected_range, 0..offset);
                assert_eq!(input.cursor_point(), Some(downstream));
                input.select_up(&SelectUp, window, cx);
                assert_eq!(input.selected_range, 0..0);
            });
        }).unwrap();
    }

    #[gpui::test]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the rows between two carets: a few, counted downwards"
    )]
    fn vertical_navigation_retains_column_across_short_and_wrapped_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        with_composer_input(cx, |input, window, cx| {
            let long = "abcdefghijklmnopqrstuvwx";
            let raw = format!("{long}\nx\n{long}");
            let landing = format!("{long}\nx\nabcdefg").len();
            for width in [1000.0, 140.0] {
                input.set_text(&raw, cx);
                input.layout_text(px(width), &window.text_style(), window);
                input.move_to(7, cx);
                let start = input.cursor_point().unwrap();
                let target = input.point_for_index(landing).unwrap();
                let rows =
                    (f32::from(target.y - start.y) / f32::from(input.line_height)).round() as usize;
                assert!(rows >= 2);
                for _ in 0..rows {
                    input.down(&Down, window, cx);
                }
                assert_eq!(input.cursor_offset(), landing, "column lost at width {width}");
                assert_eq!(input.preferred_column, Some(start.x));
                for _ in 0..rows {
                    input.up(&Up, window, cx);
                }
                assert_eq!(input.cursor_offset(), 7);
                for _ in 0..rows {
                    input.select_down(&SelectDown, window, cx);
                }
                assert_eq!(input.selected_range, 7..landing);
                for _ in 0..rows {
                    input.select_up(&SelectUp, window, cx);
                }
                assert_eq!(input.selected_range, 7..7);
                input.right(&Right, window, cx);
                assert_eq!(input.preferred_column, None);
                input.down(&Down, window, cx);
                assert!(input.preferred_column.is_some());
                input.replace_text_in_range(None, "é", window, cx);
                assert_eq!(input.preferred_column, None);
                input.undo(&Undo, window, cx);
                assert_eq!(input.text(), raw);
            }
        });
    }

    #[gpui::test]
    fn bulk_edits_and_following_typing_are_separate_undo_steps(cx: &mut gpui::TestAppContext) {
        with_composer_input(cx, |input, window, cx| {
            input.set_text("", cx);
            input.replace_text_in_range(None, "paste", window, cx);
            input.replace_text_in_range(None, "d", window, cx);
            input.undo(&Undo, window, cx);
            assert_eq!(input.text(), "paste");
            input.undo(&Undo, window, cx);
            assert_eq!(input.text(), "");
        });
    }

    #[gpui::test]
    fn rendered_word_selection_uses_pointer_hit_testing(cx: &mut gpui::TestAppContext) {
        let (_dir, handle) = composer_focus_window(cx);
        let input = handle.read_with(cx, |composer, _| composer.input.clone()).unwrap();
        input.update(cx, |input, cx| input.set_text("hello café world", cx));
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            let position = {
                let editor = input.read(cx);
                let local = editor.point_for_index(8).unwrap();
                editor.last_bounds.unwrap().origin + local + point(px(1.0), px(5.0))
            };
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    click_count: 2,
                    ..Default::default()
                }),
                cx,
            );
            assert_eq!(input.read(cx).selected_range, 6..11);
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    ..Default::default()
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    click_count: 3,
                    ..Default::default()
                }),
                cx,
            );
            assert_eq!(input.read(cx).selected_range, 0..17);
        })
        .unwrap();
    }

    #[test]
    fn multi_click_selects_words_and_lines() {
        assert_eq!(press_intent(1, false), PressIntent::PlaceCaret);
        assert_eq!(press_intent(1, true), PressIntent::ExtendSelection);
        assert_eq!(press_intent(2, false), PressIntent::Word);
        assert_eq!(press_intent(3, false), PressIntent::Line);
        assert_eq!(word_range("hello café world", 8), 6..11);
        assert_eq!(word_range("hello world", 3), 0..5);
    }

    #[gpui::test]
    fn explicit_navigation_breaks_undo_runs_but_backspace_remains_coalesced(
        cx: &mut gpui::TestAppContext,
    ) {
        with_composer_input(cx, |input, window, cx| {
            input.set_text("", cx);
            input.replace_text_in_range(None, "a", window, cx);
            input.move_to(0, cx);
            input.move_to(1, cx);
            input.replace_text_in_range(None, "b", window, cx);
            input.undo(&Undo, window, cx);
            assert_eq!(input.text(), "a");
            input.set_text("abcd", cx);
            input.backspace(&Backspace, window, cx);
            input.backspace(&Backspace, window, cx);
            input.undo(&Undo, window, cx);
            assert_eq!(input.text(), "abcd");
        });
    }

    // `$` opens skills alone and `/` commands alone, for every agent alike.
    #[test]
    fn input_wheel_scroll_uses_gpui_direction_and_clamps() {
        // Positive wheel delta moves toward the start; negative moves down.
        assert_eq!(input_scroll_offset(40.0, 20.0, 200.0, 100.0), 20.0);
        assert_eq!(input_scroll_offset(40.0, -30.0, 200.0, 100.0), 70.0);
        // Neither edge can be overscrolled.
        assert_eq!(input_scroll_offset(10.0, 50.0, 200.0, 100.0), 0.0);
        assert_eq!(input_scroll_offset(90.0, -50.0, 200.0, 100.0), 100.0);
        // Short content has no internal scroll range.
        assert_eq!(input_scroll_offset(20.0, -50.0, 80.0, 100.0), 0.0);
    }

    #[test]
    fn input_scroll_reveals_only_when_caret_leaves_viewport() {
        // A visible caret preserves the user's viewport.
        assert_eq!(input_scroll_offset_for_cursor(40.0, 60.0, 20.0, 300.0, 100.0, None), 40.0);
        // Moving above or below reveals the row with the smallest adjustment.
        assert_eq!(input_scroll_offset_for_cursor(80.0, 30.0, 20.0, 300.0, 100.0, None), 30.0);
        assert_eq!(input_scroll_offset_for_cursor(20.0, 130.0, 20.0, 300.0, 100.0, None), 50.0);
        // Revealing the final row clamps exactly to the content end.
        assert_eq!(input_scroll_offset_for_cursor(0.0, 290.0, 20.0, 300.0, 100.0, None), 200.0);
    }

    #[test]
    fn input_drag_autoscroll_is_edge_proportional_and_capped() {
        let top = 100.0;
        let bottom = 300.0;
        let line = INPUT_LINE_HEIGHT;
        assert_eq!(input_drag_scroll_delta(200.0, top, bottom, line), 0.0);
        assert_eq!(input_drag_scroll_delta(90.0, top, bottom, line), -2.0);
        assert_eq!(input_drag_scroll_delta(315.0, top, bottom, line), 3.0);
        assert_eq!(input_drag_scroll_delta(-100.0, top, bottom, line), -line);
        assert_eq!(input_drag_scroll_delta(500.0, top, bottom, line), line);
    }

    #[test]
    fn resize_reveals_only_complete_rows() {
        for visible in [0.0, 5.0, 22.0, 22.75, 30.0, 45.5, 70.0, 150.0] {
            let height = input_reveal_height(visible, 0.0, INPUT_LINE_HEIGHT, true);
            assert!(height <= visible);
            assert_eq!(height % INPUT_LINE_HEIGHT, 0.0);
        }
        // The row grid moves with scrolling; the clip still ends between rows.
        assert_eq!(input_reveal_height(39.0, 7.0, 20.0, true), 33.0);
        // Normal overflow scrolling keeps its full viewport and existing fades.
        assert_eq!(input_reveal_height(39.0, 7.0, 20.0, false), 39.0);
        assert_eq!(input_reveal_height(100.0, 0.0, 20.0, true), 100.0);
    }

    #[test]
    fn resize_keeps_text_anchored_to_the_input_origin() {
        // A fitting draft grows from one row to seven. Caret-follow must
        // never temporarily scroll earlier lines through the top clip.
        for visible in [0.0, 22.75, 60.0, 110.0, 159.25] {
            assert_eq!(
                input_scroll_offset_for_cursor(0.0, 136.5, 22.75, 159.25, visible, Some(159.25),),
                0.0
            );
        }
        // A genuinely overflowing draft keeps the same caret-follow offset
        // through every frame of the reveal, rather than chasing its height.
        for visible in [30.0, 100.0, 180.0, 240.0] {
            assert_eq!(
                input_scroll_offset_for_cursor(160.0, 377.25, 22.75, 400.0, visible, Some(240.0),),
                160.0
            );
        }
        // Deleting back to a fitting draft resets scroll immediately, even
        // while the old, larger viewport is still shrinking.
        assert_eq!(
            input_scroll_offset_for_cursor(160.0, 77.25, 22.75, 100.0, 240.0, Some(100.0),),
            0.0
        );
    }

    #[test]
    fn scroll_fade_ignores_temporary_resize_overflow() {
        for visible_height in [0.0, 20.0, 60.0, 100.0, 160.0] {
            let scroll = input_max_scroll(160.0, visible_height);
            assert_eq!(input_overflow_edges(160.0, 160.0, visible_height, scroll), (false, false));
        }
        // Deleting a capped draft disables fading immediately, even while
        // its scroll position and outer height are still settling.
        assert_eq!(input_overflow_edges(100.0, 100.0, 240.0, 80.0), (false, false));
    }

    #[test]
    fn scroll_fade_tracks_real_overflow_edges() {
        for (scroll, top, bottom) in [(0.0, false, true), (80.0, true, true), (160.0, true, false)]
        {
            assert_eq!(input_overflow_edges(400.0, 240.0, 240.0, scroll), (top, bottom));
        }
    }
}
