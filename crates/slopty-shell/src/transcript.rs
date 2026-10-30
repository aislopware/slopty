// Derived from zeron (MIT, github.com/zeronsh/zeron @ 46d4e42), see THIRD_PARTY_NOTICES.md
//! The conversation view: virtualized transcript with block-granularity rows,
//! the follow rule, tool-group folding, and streaming markdown.
//!
//! Row model (docs/research/mugen-pretext.md §3):
//! - one row per BLOCK: user message = one bubble row; assistant messages split
//!   into one row per markdown top-level block, plus consecutive-tool groups
//!   (agent/spawn chips split out so they never collapse) and input/error chips;
//! - stable row ids `{msgId}#{partId}.{blockIx}` / `{msgId}#g{groupIx}` — LIVE
//!   (streaming) entries split per block exactly like completed ones (the list
//!   virtualizes them, so a fading live reply re-renders only its visible tail
//!   each frame — flat cost in the reply length); on completion each block row
//!   keeps its id, so row identity is continuous and nothing flickers;
//! - rows are cached per entry keyed by a content fingerprint — only changed
//!   messages rebuild (the anti-"streaming stutter" trick);
//! - row-set changes diff by (id, version) into one minimal `splice`.
//!
//! slopty: the follow rule (`docs/decisions/inspector.md`, "Follow and anchor"): the list
//! follows its end (GPUI's `FollowMode::Tail`) only while within [`FOLLOW_PX`] of it, and lets
//! go only on the person's own move: a wheel or drag up, a scroll key, a jump, or a press or a
//! selection in a row. Otherwise the first row in view holds its place by its key, whatever
//! lands above or below it; scrolling back to the end follows again, and while it does not, a
//! pill says how many rows wait below.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, ClipboardItem, Context, Entity, FollowMode, ListAlignment, ListOffset,
    ListScrollEvent, ListState, MouseButton, MouseMoveEvent, MouseUpEvent, ObjectFit, PathBuilder,
    Pixels, Point, SharedString, StyledImage as _, StyledText,
    Subscription, Task, TextRun, Window, canvas, div, img, list, point, prelude::*, px, size,
};

use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, SubagentStatus};
use slopty_view_proto::ToolCall;

use crate::markdown::mermaid_cache::{self, MermaidCache};
use crate::markdown::parser::{
    Block, BlockTree, IncrementalParser, InlineRun, InlineStyle, parse_full,
};
use crate::markdown::render::{self, RenderCache, RenderOptions};
use crate::markdown::veil::RowVeil;
use crate::motion::{self, AnimationExt as _};
use crate::state::AppState;
use crate::syntax_cache::{DocumentHighlightKey, SyntaxHighlightCache};
use crate::settings::widgets::ActionHint as _; // slopty: tooltips name their key.
use crate::theme::Theme;
use slopty_syntax::LanguageId as Lang;
use crate::motion::HoverFade as _;

// slopty: Slopty's own rows, and find in thread.
mod context_menu; // slopty: right-click menus in the chat.
mod extras;
// slopty: background work in the transcript (task notices, notices, a call's facts) and a
// subagent's card, which opens in place.
mod md_images;
mod pages;
mod sticky; // slopty: the prompt a long turn answers, held at the top.
mod subagents;
#[cfg(test)]
mod surface_tests; // slopty: the transcript's own rows, read by assistive tech.
pub mod work;
pub(crate) mod find;
mod jump; // slopty: the pill that says what waits below and goes back to it.
pub use extras::{claude_too_old, serve_terminal_runs};
pub use find::{FindInThread, FindNext, FindPrevious};

// ---------------------------------------------------------------------------
// Constants (mugen ports)
// ---------------------------------------------------------------------------

/// List overdraw beyond the viewport.
pub const OVERDRAW_PX: f32 = 320.0;
/// slopty: the jump pill shows, with no new row below, once the view is this far from the end.
pub const SCROLL_BUTTON_THRESHOLD_PX: f32 = 320.0;
/// Bound session-local viewport memory independently of total chat history.
const MAX_SAVED_VIEWPORTS: usize = 256;
/// Text-selection edge scrolling runs only during a drag. A 24 ms cadence is
/// smooth enough to track text while avoiding a permanent animation-frame loop
/// on low-end devices.
const SELECTION_SCROLL_TICK_MS: u64 = 24;
const SELECTION_SCROLL_EDGE_PX: f32 = 36.0;
// slopty: the working line's words, in its muted ink, read at a glance beside the reply (Claude
// desktop's status line); its figures (the timer) a step smaller.
const WORKING_LEAD_SIZE: f32 = 13.0;
const WORKING_FIGURES_SIZE: f32 = 12.0;
const SELECTION_SCROLL_MAX_STEP_PX: f32 = 24.0;
/// Activity row height / gap — analytic, so fold heights need no measurement.
/// Ordinary tools place their icon on the rail; subagents retain a 30px card.
/// Rows stack without a gap so the rail continues alongside expanded output.
pub const CHIP_HEIGHT: f32 = 38.0;
pub const CHIP_GAP: f32 = 0.0;
pub const CHIP_CARD_HEIGHT: f32 = 30.0;
/// Inner height of the chip header: [`CHIP_CARD_HEIGHT`] is the card's
/// border-box (explicit `h` in gpui includes the 1px border), so a 30px
/// header inside a 30px bordered card clips 2px off the bottom and every
/// glyph/icon reads high (user report).
const CHIP_HEADER_HEIGHT: f32 = CHIP_CARD_HEIGHT - 2.0;
/// Child labels and expanded text share one edge beneath the compact summary.
/// The child gutter reserves room for
/// a longer elbow, a 4px break before the icon, and an 8px icon-to-text gap.
const ACTIVITY_GUTTER_WIDTH: f32 = 48.0;
const ACTIVITY_TEXT_GAP: f32 = 8.0;
const ACTIVITY_TRUNK_X: f32 = 12.5;
const ACTIVITY_BEND_RADIUS: f32 = 6.0;
const ACTIVITY_BRANCH_END_X: f32 = 28.0;
const ACTIVITY_ICON_LEFT: f32 = 32.0;
/// slopty: where an activity line's words start: a group's summary after its chevron, a lone
/// call after its icon, a notice after its dot (#2, #4).
const ACTIVITY_TITLE_X: f32 = 28.0;
/// slopty: every bordered card in the transcript's column (code, files, subagents, pages,
/// images, requests) shares one corner and the theme's hairline.
pub(crate) const TRANSCRIPT_CARD_RADIUS: f32 = 10.0;
/// slopty: the copy span's key for entry `entry`'s message.
fn copy_message_key(entry: &str) -> String {
    format!("copy-message-{entry}")
}

/// slopty: the lane under an entry's last row: its hover strip (timestamp, copy) and, under the
/// last row of a live turn, the working line in the same room.
pub(crate) const ROW_LANE_HEIGHT: f32 = Theme::SPACE_SM + Theme::SPACE_MD * 2.0;
/// slopty: the step before a person's prompt, which heads its turn (#1).
const PROMPT_GAP: f32 = Theme::SPACE_LG + Theme::SPACE_SM;
const ACTIVITY_ICON_SIZE: f32 = 16.0;
const TOOL_TEXT_SIZE: f32 = 12.0;
const TOOL_LABEL_SIZE: f32 = TOOL_TEXT_SIZE;
const TOOL_LABEL_LINE_HEIGHT: f32 = 18.0;
const TOOL_GROUP_HEADER_HEIGHT: f32 = 26.0;

/// slopty: a tool group's summary row as drawn: a finger's row on a touch screen (#23).
fn tool_group_header_height() -> f32 {
    crate::surface_chrome::touch_row_height(TOOL_GROUP_HEADER_HEIGHT)
}

/// Compact rows retain the analytic heights used by row and group folds.
const TOOL_TREE_ROW_HEIGHT: f32 = 32.0;
/// slopty: a jump's landing flash fading out (Claude's `code-reply-flash`, 1.2 s ease-out, #13).
const LANDING_FLASH: motion::MotionSpec = motion::MotionSpec::new(1_200, motion::EASE_OUT);
/// slopty: a bubble not yet taken by the agent is tinted, not faded (Amp): 14 % of the accent
/// mixed into its fill, as a queued row's; it settles over 200 ms once taken.
pub(crate) const QUEUED_TINT: f32 = 0.14;
const QUEUED_SETTLE: motion::MotionSpec = motion::MotionSpec::new(200, motion::EASE_OUT);

/// slopty: `fill` with `tint` of the accent mixed in, for what waits to be sent.
pub(crate) fn queued_fill(fill: gpui::Hsla, accent: gpui::Hsla, tint: f32) -> gpui::Hsla {
    motion::mix(fill, accent, tint)
}

/// Each newly appended row reveals quickly enough to read as a continuous log
/// rather than a stack of discrete pop-ins.
const TOOL_ROW_REVEAL: motion::MotionSpec = motion::MotionSpec::new(360, motion::EASE_OUT_EXPO);
/// The connector draws briskly, then eases into the branch tip so its arrival
/// remains visible without feeling mechanically linear.
const TOOL_CONNECTOR_REVEAL: motion::MotionSpec =
    motion::MotionSpec::new(480, motion::EASE_OUT_QUINT);
const TOOL_FIRST_ROW_DELAY_MS: u64 = 90;
const TOOL_ROW_STAGGER_MS: u64 = 65;

/// Signed list scroll step for a pointer near a viewport edge.
///
/// GPUI list offsets increase toward the document bottom. The quadratic ramp
/// keeps entry into the edge zone gentle and reaches full speed at the edge.
pub(crate) fn selection_scroll_step(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    let height = f32::from(bounds.size.height);
    if height <= 0.0 {
        return 0.0;
    }
    let edge = SELECTION_SCROLL_EDGE_PX.min(height / 3.0);
    if edge <= 0.0 {
        return 0.0;
    }
    let y = f32::from(position.y);
    let top = f32::from(bounds.top());
    let bottom = f32::from(bounds.bottom());
    let scaled = |penetration: f32| {
        let t = (penetration / edge).clamp(0.0, 1.0);
        SELECTION_SCROLL_MAX_STEP_PX * t * t
    };
    if y < top + edge {
        -scaled(top + edge - y)
    } else if y > bottom - edge {
        scaled(y - (bottom - edge))
    } else {
        0.0
    }
}
const CHIPS_TOP_PAD: f32 = 2.0;
/// How long a user fold toggle keeps its height tween armed: the RESIZE
/// spec's 200ms plus margin. Past this the fold renders statically — an armed
/// tween replays on remount, i.e. on every scroll-back-into-view.
const FOLD_TWEEN_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
/// A user prompt renders at most this many wrapped lines until expanded. A
/// pasted log or file drops into the transcript as one endless slab otherwise
/// (user report) — past the cap the bubble clips and grows a chevron.
pub const USER_COLLAPSED_LINES: usize = 5;
/// The user bubble's line box.
pub const USER_LINE_HEIGHT: f32 = 22.0;
/// Lines a folded prompt's Markdown lays out past the fold, under its fade.
const USER_FOLD_OVERDRAW_LINES: usize = 2;
/// Conservative first-frame soft-wrap proxy for the fixed-width long-prompt
/// bubble. The final decision uses the wrapped `StyledText` layout, but this
/// fallback lets clearly long prompts render their affordance immediately
/// before that first layout has completed.
pub const USER_COLLAPSE_CHARS: usize = 400;
/// Vertical separation before the plain expand/collapse link.
const USER_TOGGLE_GAP: f32 = 4.0;
/// slopty: the expander's line, at its 12 pt (#6).
const USER_TOGGLE_LINE_HEIGHT: f32 = 18.0;
/// User-bubble attachment thumbnails (user-attachments.tsx): 112×80 thumbs in
/// a wrapping strip. Fixed thumbnail sizes keep load-state flips from
/// shifting the virtualizer.
pub const ATT_THUMB_W: f32 = 112.0;
pub const ATT_THUMB_H: f32 = 80.0;

/// slopty: the view follows its end only while within this of it (`inspector.md`, "Follow and
/// anchor").
pub const FOLLOW_PX: f32 = 2.0;
/// slopty: a row a jump lands on rests this far below the transcript viewport's top (`rail.rs`):
/// the transcript lays out inside its pane, which clears the window's chrome, so the inset is
/// only its own top scroll edge, and the row rests where that fade ends.
pub(crate) const OWN_SEND_TOP_INSET_PX: f32 = Theme::TRANSCRIPT_FADE_BAND;
/// slopty: what a markdown block says, as plain text: its texts as drawn (a relabelled link by
/// its label), in paint order, a line each.
fn block_words(block: &crate::markdown::Block) -> String {
    let mut texts = Vec::new();
    crate::markdown::find::block_texts(&crate::markdown::chat_links::as_drawn(block), &mut texts);
    texts.join("\n")
}

/// slopty: an agent of `harness` by name, "Agent" when the chat names none (#10).
fn agent_name(harness: Option<slopty_view_proto::HarnessId>) -> SharedString {
    let Some(harness) = harness else { return "Agent".into() };
    crate::settings::SKILL_COMPLETION_HARNESSES
        .iter()
        .find(|(id, _)| *id == harness)
        .map_or_else(|| harness.as_str().into(), |(_, name)| (*name).into())
}

/// slopty: the focus of a strip's Copy, and what redraws the strip as it gains or loses it.
struct StripFocus {
    copy: gpui::FocusHandle,
    _redraw: [gpui::Subscription; 2],
}

/// slopty: `inner` as the start of a message `who` said, to the accessibility tree.
fn message_article(row: &SharedString, who: impl Into<SharedString>, inner: AnyElement) -> AnyElement {
    div()
        .id(SharedString::from(format!("{row}-message")))
        .role(gpui::Role::Article)
        .aria_label(who)
        .child(inner)
        .into_any_element()
}

/// slopty: where a transcript fit to its rows tells their height: the card it is open in.
#[derive(Clone)]
struct Fit {
    card: gpui::WeakEntity<Transcript>,
    /// The card's row there.
    row: SharedString,
    height: Rc<Cell<Option<f32>>>,
}

impl Fit {
    /// After the list's layout, its rows' height from `list`: the card is drawn again when it
    /// moves, so the card's height is always the one this frame measured.
    fn measure(self, list: ListState) -> AnyElement {
        gpui::canvas(
            move |_, _, cx| {
                let now = rows_height(&list);
                let moved = match (self.height.get(), now) {
                    (Some(had), Some(now)) => (had - now).abs() > 0.5,
                    (had, now) => had.is_some() != now.is_some(),
                };
                if moved {
                    self.height.set(now);
                    let (card, row) = (self.card.clone(), self.row.clone());
                    cx.defer(move |cx| {
                        card.update(cx, |card, cx| card.row_resized(&row, cx)).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0()
        .into_any_element()
    }
}

/// slopty: the height `list`'s rows take as last laid out, to its last row's end; past its
/// viewport, more than any room (`f32::MAX`), as rows not yet laid out are not measured, so the
/// card takes its whole room and measures again; `None` before it laid out any.
fn rows_height(list: &ListState) -> Option<f32> {
    let count = list.item_count();
    let last = count.checked_sub(1)?;
    let viewport = list.viewport_bounds();
    let shown = f32::from(viewport.size.height);
    if shown <= 0.0 {
        return None;
    }
    let past = f32::from(list.max_offset_for_scrollbar().y);
    if past > 0.5 {
        return Some(f32::MAX);
    }
    let end = list.bounds_for_item(last)?;
    Some(f32::from(end.bottom() - viewport.top()))
}






// ---------------------------------------------------------------------------
// Row model (pure)
// ---------------------------------------------------------------------------

/// What a chip inside a [`RowKind::ToolGroup`] represents. `Call` items come
/// from doc tool parts; `Thought` (reasoning) is UI-synthesized in
/// [`rows_for_entry`] — the doc never emits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolItemKind {
    Call,
    Thought,
}

/// One tool invocation inside a group row.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolItem {
    /// Stable document part identity, independent of arrival order.
    pub part_id: String,
    pub call: ToolCall,
    pub is_error: bool,
    pub resolved: bool,
    /// Expandable detail: a code-block of output lines, or a real diff
    /// section rendered by the changes pane's component (ACP harnesses).
    /// Precomputed here because rows are cached by fingerprint — diffing and
    /// tokenizing per paint would run on every scroll frame.
    pub detail: Option<Arc<ToolDetail>>,
    /// Expandable full-invocation block: the complete tool call (whole
    /// command / pattern / URL / input JSON) that the chip header collapses
    /// to one truncated line. Rendered above `detail` in the open card.
    /// Precomputed for the same reason as `detail`.
    pub invocation: Option<Arc<ToolDetail>>,
    /// Sidecar key of the full output (chat2-sync A3) — the doc carries only
    /// a one-line summary; expanding offers a lazy "Show full output" fetch.
    pub output_ref: Option<SharedString>,
    /// Full-output size, for the affordance label ("Show full output (12 KB)").
    pub output_bytes: Option<u64>,
    /// Sidecar key of the full diff (doc carries only per-file stats).
    pub diff_ref: Option<SharedString>,
    /// The spawned SUBAGENT's doc id — the chip IS the index (there is no
    /// listing endpoint); with it the chip offers "Open subagent".
    pub subagent_ref: Option<SharedString>,
    /// Subagent lifecycle, distinct from `resolved` (eager-done: the spawn
    /// tool's own result lands while the subagent still runs).
    pub subagent_status: Option<SubagentStatus>,
    /// One-line live tail — LEGACY docs only (new runs stopped folding it;
    /// per-delta header rewrites read as noise). Never rendered; still
    /// fingerprinted so an old doc's chips re-splice correctly.
    pub subagent_tail: Option<SharedString>,
    /// `Call` is a real doc tool invocation; `Thought` (a reasoning part
    /// riding the tool group — the thought process belongs inside the
    /// combined "Ran N commands" accordion, opening/closing with the same
    /// tween) is synthesized in [`rows_for_entry`], never from a doc tool
    /// part. A `Thought`'s text is the `detail`; `resolved == false` means it
    /// is still streaming.
    pub kind: ToolItemKind,
    /// Image files the call read, wrote or printed (as reported; relative
    /// paths resolve against the chat cwd). Expanding the chip previews them.
    pub images: Arc<[SharedString]>,
    // slopty: what the call says beyond zeron's chip, in the TUI's words (`transcript/work.rs`).
    pub facts: Option<Arc<slopty_view_doc::ToolFacts>>,
}

/// Subagent spawn chips — [`ToolCall::is_subagent_spawn`], the shared genus
/// every driver decodes its spawn tool into. These stay out of the
/// collapsible "Called N tools" wrap so a running subagent is visible
/// without opening the fold.
fn is_agent_call(call: &ToolCall) -> bool {
    call.is_subagent_spawn()
}

/// The chip's GENUS is the call itself, never the ref: docs written before
/// the claude-driver fix carry stray `subagent_ref`s on ordinary Run chips
/// (a background shell's `task_notification` was mis-tagged as subagent
/// traffic), and honoring the ref alone turned those Runs into spawn chips
/// that opened empty, never-created subagent docs.
fn is_agent_tool(item: &ToolItem) -> bool {
    is_agent_call(&item.call)
}

/// A chip renders as the spawn LINK (whole-card click → subagent tab) only
/// when an agent call has actually been bound to its doc.
// slopty: the subagent's card (`transcript/subagents.rs`), once its run is known: bound to its
// chat, or with its progress from the view model.
fn is_spawn_link(item: &ToolItem) -> bool {
    is_agent_call(&item.call)
        && (item.subagent_ref.is_some()
            || item
                .facts
                .as_ref()
                .is_some_and(|facts| facts.subagent_progress.is_some()))
}

/// Ordinary tool groups fold behind a summary header; agent/spawn chips
/// render as their own always-open row.
fn tool_group_collapses(tools: &[ToolItem]) -> bool {
    tools.iter().any(|t| !is_agent_tool(t))
}

/// slopty: whether a group folds behind its header, and whether it is one call shown as itself.
/// Agent/spawn chips never fold (a running subagent stays visible). A group of one call (or one
/// thought) shows the call itself ("Read main.rs"), not a summary to open to learn it ("Read 1
/// file ›") (#2).
fn group_shape(tools: &[ToolItem]) -> (bool, bool) {
    let collapses = tool_group_collapses(tools);
    let solo = collapses && tools.len() == 1 && !is_spawn_link(&tools[0]);
    (collapses, solo)
}

/// Column budget for soft-wrapping thought text into detail lines. The
/// detail body is preformatted (no element wrapping), so the wrap happens
/// here — conservative enough to fit the card at typical transcript widths.
const THOUGHT_WRAP_COLS: usize = 96;

/// Flatten a thought's parsed markdown into wrapped, STYLED detail lines —
/// inline markers render as real styling (bold/italic/code/links) instead of
/// literal `**` glyphs; blocks flatten structurally (headings as prose, list
/// bullets, quote bars, verbatim code lines). Every line is one fixed-height
/// row, so the detail height stays analytic (lines × [`OUTPUT_LINE_HEIGHT`])
/// and the group's fold tween keeps working without measurement.
fn thought_lines(tree: &BlockTree) -> Vec<Vec<InlineRun>> {
    let mut out: Vec<Vec<InlineRun>> = Vec::new();
    for top in &tree.blocks {
        if !out.is_empty() {
            // One blank separator row between top-level blocks (the old
            // plain-text wrap kept paragraph gaps the same way).
            out.push(Vec::new());
        }
        thought_block_lines(&top.block, 0, &mut out);
    }
    while out
        .last()
        .is_some_and(|l| l.iter().all(|r| r.text.trim().is_empty()))
    {
        out.pop();
    }
    out
}

/// The slot-0 indent run every emitted thought line opens with (possibly
/// empty). List/quote handlers rewrite it in place to plant markers/bars, so
/// it must exist even at zero indent.
fn indent_run(indent: usize) -> Vec<InlineRun> {
    vec![InlineRun {
        text: " ".repeat(indent),
        style: InlineStyle::default(),
    }]
}

/// Append text to a line's run list, merging into the tail run when styles
/// match (keeps run counts small for the shaper).
fn push_styled(line: &mut Vec<InlineRun>, text: &str, style: &InlineStyle) {
    if text.is_empty() {
        return;
    }
    match line.last_mut() {
        Some(last) if last.style == *style => last.text.push_str(text),
        _ => line.push(InlineRun {
            text: text.to_owned(),
            style: style.clone(),
        }),
    }
}

/// Close a wrapped line: the slot-0 indent run in front (see [`indent_run`]).
fn finish_line(indent: usize, mut line: Vec<InlineRun>) -> Vec<InlineRun> {
    let mut full = indent_run(indent);
    full.append(&mut line);
    full
}

/// Word-wrap styled runs at the thought column budget. Char-counted like
/// every detail wrap — block heights must stay analytic — with words glued
/// across style boundaries (`**bold**tail` wraps as one unit), separator
/// spaces riding the preceding run, and pathological overlong tokens
/// hard-split at the budget. Hard breaks (`\n` runs) split into
/// separately-wrapped segments.
fn wrap_styled_runs(runs: &[InlineRun], indent: usize, out: &mut Vec<Vec<InlineRun>>) {
    let budget = THOUGHT_WRAP_COLS.saturating_sub(indent).max(16);
    let mut segments: Vec<Vec<InlineRun>> = vec![Vec::new()];
    for run in runs {
        // Inline math shows its `$tex$` source in the code style.
        let math;
        let run = if run.style.math {
            math = InlineRun {
                text: format!("${}$", run.text),
                style: InlineStyle {
                    code: true,
                    math: false,
                    ..run.style.clone()
                },
            };
            &math
        } else {
            run
        };
        for (ix, piece) in run.text.split('\n').enumerate() {
            if ix > 0 {
                segments.push(Vec::new());
            }
            if !piece.is_empty() {
                segments.last_mut().unwrap().push(InlineRun {
                    text: piece.to_owned(),
                    style: run.style.clone(),
                });
            }
        }
    }
    for segment in segments {
        // Tokens: maximal non-whitespace piece lists, glued across run
        // boundaries so a word split by styling never wraps mid-word.
        let mut tokens: Vec<Vec<InlineRun>> = Vec::new();
        let mut in_token = false;
        for run in &segment {
            let text = run.text.as_str();
            let mut pos = 0;
            while pos < text.len() {
                let rest = &text[pos..];
                let ws = rest.chars().next().is_some_and(char::is_whitespace);
                let end = rest
                    .char_indices()
                    .find(|(_, c)| c.is_whitespace() != ws)
                    .map_or(text.len(), |(i, _)| pos + i);
                if ws {
                    in_token = false;
                } else {
                    if !in_token {
                        tokens.push(Vec::new());
                        in_token = true;
                    }
                    push_styled(tokens.last_mut().unwrap(), &text[pos..end], &run.style);
                }
                pos = end;
            }
        }
        let mut line: Vec<InlineRun> = Vec::new();
        let mut len = 0usize;
        for token in tokens {
            let tok_len: usize = token.iter().map(|r| r.text.chars().count()).sum();
            if tok_len > budget {
                // Hard-split a pathological token at the budget.
                if len > 0 {
                    out.push(finish_line(indent, std::mem::take(&mut line)));
                    len = 0;
                }
                for piece in token {
                    let mut chars = piece.text.chars();
                    loop {
                        let chunk: String = chars.by_ref().take(budget - len).collect();
                        if chunk.is_empty() {
                            break;
                        }
                        len += chunk.chars().count();
                        push_styled(&mut line, &chunk, &piece.style);
                        if len == budget {
                            out.push(finish_line(indent, std::mem::take(&mut line)));
                            len = 0;
                        }
                    }
                }
                continue;
            }
            if len > 0 && len + 1 + tok_len > budget {
                out.push(finish_line(indent, std::mem::take(&mut line)));
                len = 0;
            }
            if len > 0 {
                if let Some(last) = line.last_mut() {
                    last.text.push(' ');
                }
                len += 1;
            }
            for piece in token {
                push_styled(&mut line, &piece.text, &piece.style);
            }
            len += tok_len;
        }
        if len > 0 {
            out.push(finish_line(indent, line));
        }
    }
}

/// One markdown block into thought detail lines, `indent` spaces deep.
fn thought_block_lines(block: &Block, indent: usize, out: &mut Vec<Vec<InlineRun>>) {
    match block {
        // slopty: a heading in a thought reads as its prose does, at the body's size and weight
        // (opencode's reasoning): a thought never outranks the answer.
        Block::Paragraph { runs } | Block::Heading { runs, .. } => wrap_styled_runs(runs, indent, out),
        // Interim: display math shows its TeX source like a code block.
        Block::CodeBlock { code, .. } | Block::Math { tex: code } => {
            let style = InlineStyle {
                code: true,
                ..InlineStyle::default()
            };
            for line in code.lines() {
                for chunk in wrap_cols(line, THOUGHT_WRAP_COLS.saturating_sub(indent).max(16)) {
                    let mut row = indent_run(indent);
                    if !chunk.is_empty() {
                        row.push(InlineRun {
                            text: chunk.to_string(),
                            style: style.clone(),
                        });
                    }
                    out.push(row);
                }
            }
        }
        Block::List {
            ordered_start,
            items,
        } => {
            // Tight rendering: no blank rows inside a list.
            for (ix, item) in items.iter().enumerate() {
                let marker = match ordered_start {
                    Some(start) => format!("{}. ", start + ix as u64),
                    None => "• ".to_string(),
                };
                let inner = indent + marker.chars().count();
                let mark = out.len();
                for child in item {
                    thought_block_lines(child, inner, out);
                }
                if out.len() == mark {
                    // An empty item still shows its marker.
                    out.push(indent_run(inner));
                }
                // The item's first line trades its indent spaces for the
                // marker (the slot-0 run is always the indent).
                if let Some(first) = out[mark].first_mut() {
                    first.text = format!("{}{marker}", " ".repeat(indent));
                }
            }
        }
        Block::BlockQuote { children } => {
            let mark = out.len();
            for (ix, child) in children.iter().enumerate() {
                if ix > 0 {
                    out.push(Vec::new());
                }
                thought_block_lines(child, indent + 2, out);
            }
            // Trade the two quote-indent spaces for the bar on every quoted
            // line — replace, not overwrite: nested list handlers already
            // planted markers after their own deeper indent.
            for line in &mut out[mark..] {
                if let Some(first) = line.first_mut()
                    && first.text.len() >= indent + 2
                {
                    first.text.replace_range(indent..indent + 2, "│ ");
                }
            }
        }
        Block::Table { header, rows, .. } => {
            // A thought is a record, not a layout surface: cells joined with
            // a dot separator, header bold — no column machinery.
            let join = |cells: &[Vec<InlineRun>], bold: bool| -> Vec<InlineRun> {
                let mut line: Vec<InlineRun> = Vec::new();
                for (ix, cell) in cells.iter().enumerate() {
                    if ix > 0 {
                        push_styled(&mut line, " · ", &InlineStyle::default());
                    }
                    for r in cell {
                        let mut r = r.clone();
                        r.style.bold |= bold;
                        line.push(r);
                    }
                }
                line
            };
            wrap_styled_runs(&join(header, true), indent, out);
            for row in rows {
                wrap_styled_runs(&join(row, false), indent, out);
            }
        }
        Block::Rule => {
            let mut row = indent_run(indent);
            row.push(InlineRun {
                text: "———".into(),
                style: InlineStyle::default(),
            });
            out.push(row);
        }
    }
}

/// A reasoning part as a tool-group chip: "Thought process" header over the
/// thought's markdown flattened into styled detail lines (analytic height —
/// the group's fold tween needs it; see [`thought_lines`]). Capped like tool
/// outputs, with the counted tail. `live` = the part is still streaming
/// (chip defaults open).
fn thought_item(part_id: &str, tree: &BlockTree, live: bool) -> ToolItem {
    let mut lines = thought_lines(tree);
    let truncated_by = lines.len().saturating_sub(OUTPUT_DETAIL_MAX_LINES);
    if truncated_by > 0 {
        // Keep the TAIL while streaming (the fresh thinking is the signal);
        // settled thoughts keep the head like tool outputs do.
        if live {
            lines.drain(..truncated_by);
            // The cut can land on a block separator — drop the orphan blank.
            while lines
                .first()
                .is_some_and(|l| l.iter().all(|r| r.text.trim().is_empty()))
            {
                lines.remove(0);
            }
        } else {
            lines.truncate(OUTPUT_DETAIL_MAX_LINES);
        }
    }
    ToolItem {
        part_id: part_id.into(),
        call: ToolCall::Unknown {
            name: "Thought process".into(),
            input: None,
        },
        is_error: false,
        resolved: !live,
        detail: (!lines.is_empty()).then(|| {
            Arc::new(ToolDetail::Thought {
                lines,
                truncated_by,
            })
        }),
        invocation: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
        kind: ToolItemKind::Thought,
        images: Arc::new([]),
        facts: None,
    }
}

/// A chip's expandable detail payload.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolDetail {
    /// Command/tool output as a code block: verbatim lines (indentation
    /// intact), capped at [`OUTPUT_DETAIL_MAX_LINES`] with a counted tail.
    Output {
        lines: Vec<SharedString>,
        truncated_by: usize,
        /// slopty: the first of `lines` that is the command's standard error, drawn in the
        /// danger colour after its output, as the TUI does.
        stderr_from: Option<usize>,
    },
    /// A thought's markdown, pre-flattened into wrapped STYLED lines — one
    /// fixed-height row each, so the height stays analytic like `Output`
    /// while inline markers render as real styling ([`thought_lines`]).
    Thought {
        lines: Vec<Vec<InlineRun>>,
        truncated_by: usize,
    },
    /// A file diff, in the changes pane's model: hunks with 3 lines of
    /// context, dual line numbers, and (for recognized languages) syntax
    /// tokens — rendered by `changes::render_file_body`.
    Diff {
        file: Arc<crate::changes::FileDiff>,
        old_text: Option<Arc<str>>,
        new_text: Option<Arc<str>>,
    },
    /// Per-file `+N −N` stat rows — what the thin doc keeps of an edit
    /// (chat2-sync A1). The full diff upgrades this to [`ToolDetail::Diff`]
    /// via the sidecar fetch.
    Stats {
        stats: Arc<Vec<slopty_view_doc::ToolDiffStat>>,
    },
}

/// Max verbatim output lines per chip before the counted tail row.
pub const OUTPUT_DETAIL_MAX_LINES: usize = 24;

/// Max diff lines an inline tool-diff detail renders — the detail is one
/// stacked element inside its transcript row, so it must stay bounded
/// (~600 lines ≈ 12.6k px, several screens of context before the cut).
pub const DIFF_DETAIL_MAX_LINES: usize = 600;

/// Per-line height of an output detail block (diff blocks use the changes
/// pane's own [`crate::changes::DIFF_LINE_HEIGHT`]).
pub const OUTPUT_LINE_HEIGHT: f32 = 18.0;

/// Vertical padding of an output detail body (py(6) × 2).
const OUTPUT_BODY_PAD: f32 = 12.0;

/// The hairline between an expanded chip's header row and its detail body.
const DETAIL_SEPARATOR: f32 = 1.0;

/// Build a tool part's expandable detail. A diff wins over raw output (it is
/// the more structured record of the same action); post-strip docs carry diff
/// STATS instead of inline diff text, which win the same way.
pub fn tool_detail(
    output: Option<&str>,
    diff: Option<&slopty_view_proto::ToolDiff>,
    diff_stats: Option<&[slopty_view_doc::ToolDiffStat]>,
) -> Option<ToolDetail> {
    if let Some(diff) = diff {
        let mut file = diff_to_file(diff);
        if file.hunks.is_empty() {
            return None;
        }
        // A transcript diff renders as one stacked element inside its row —
        // cap it so a whole-file rewrite (or fetched full-diff blob) can't
        // build tens of thousands of elements per frame. The changes pane
        // has no such cap; it virtualizes per line.
        crate::changes::truncate_file_lines(&mut file, DIFF_DETAIL_MAX_LINES);
        return Some(ToolDetail::Diff {
            file: Arc::new(file),
            old_text: diff.old_text.as_deref().map(Arc::from),
            new_text: Some(Arc::from(diff.new_text.as_str())),
        });
    }
    if let Some(stats) = diff_stats.filter(|s| !s.is_empty()) {
        return Some(ToolDetail::Stats {
            stats: Arc::new(stats.to_vec()),
        });
    }
    let output = output?;
    let mut lines = output_lines(output); // slopty
    // Trim trailing blank output lines so the block hugs its content.
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let truncated_by = lines.len().saturating_sub(OUTPUT_DETAIL_MAX_LINES);
    lines.truncate(OUTPUT_DETAIL_MAX_LINES);
    Some(ToolDetail::Output {
        lines,
        truncated_by,
        stderr_from: None,
    })
}

/// slopty: an output's lines as its block draws them: a line of JSON pretty-printed, as the
/// TUI's output does.
fn output_lines(output: &str) -> Vec<SharedString> {
    output
        .lines()
        .flat_map(|line| match pretty_json(line) {
            Some(pretty) => pretty.lines().map(output_line).collect::<Vec<_>>(),
            None => vec![output_line(line)],
        })
        .collect()
}

/// Longest line of JSON an output pretty-prints, as the TUI's.
const PRETTY_JSON_MAX_CHARS: usize = 10_000;

/// slopty: `line` pretty-printed when it is one JSON object or array on its own, as the TUI
/// shows a tool's JSON output.
fn pretty_json(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let framed = (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'));
    if !framed || trimmed.len() > PRETTY_JSON_MAX_CHARS || trimmed.len() < 3 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pretty = serde_json::to_string_pretty(&value).ok()?;
    (pretty.lines().count() > 1).then_some(pretty)
}

/// Lines of a command's standard error a chip shows under its output.
const STDERR_MAX_LINES: usize = 10;

/// slopty: `detail` with the command's standard error after its output, marked to draw in the
/// danger colour; an output of standard error alone when there was no other.
pub fn with_stderr(detail: Option<ToolDetail>, stderr: Option<&str>) -> Option<ToolDetail> {
    let Some(stderr) = stderr.filter(|s| !s.trim().is_empty()) else { return detail };
    let mut err: Vec<SharedString> = stderr.lines().map(output_line).collect();
    let more = err.len().saturating_sub(STDERR_MAX_LINES);
    err.truncate(STDERR_MAX_LINES);
    if more > 0 {
        err.push(SharedString::from(format!("… {more} more lines of standard error")));
    }
    match detail {
        Some(ToolDetail::Output { mut lines, truncated_by, .. }) => {
            let from = lines.len();
            lines.extend(err);
            Some(ToolDetail::Output { lines, truncated_by, stderr_from: Some(from) })
        }
        None => Some(ToolDetail::Output { lines: err, truncated_by: 0, stderr_from: Some(0) }),
        other => other,
    }
}

/// slopty: the URL a line of output names, when it names one: the TUI makes it a link.
fn line_url(line: &str) -> Option<(usize, &str)> {
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let rest = line.get(start..)?;
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']'))
        .unwrap_or(rest.len());
    let url = rest.get(..end)?.trim_end_matches(['.', ',', ';', ':']);
    (url.len() > "https://".len()).then_some((start, url))
}

/// Chars kept per output line. Lines clip rather than wrap, but each one is
/// still shaped whole for selection — a minified megabyte must not be.
const OUTPUT_LINE_MAX_CHARS: usize = 8 * 1024;

/// One verbatim output line, cut to [`OUTPUT_LINE_MAX_CHARS`] with an ellipsis.
fn output_line(line: &str) -> SharedString {
    match line.char_indices().nth(OUTPUT_LINE_MAX_CHARS) {
        Some((cut, _)) => SharedString::from(format!("{}…", &line[..cut])),
        None => SharedString::from(line.to_owned()),
    }
}

/// Columns at which an invocation line soft-wraps into continuation lines.
/// The wrap is char-counted, not measured — block heights must be analytic —
/// so the budget is sized to fit the narrowest useful transcript pane.
pub const CALL_WRAP_COLS: usize = 80;

/// Soft-wrap one raw line into [`CALL_WRAP_COLS`]-char chunks so a long
/// single-line command stays fully readable instead of ellipsizing.
fn wrap_cols(line: &str, cols: usize) -> Vec<SharedString> {
    if line.chars().count() <= cols {
        return vec![SharedString::from(line.to_owned())];
    }
    line.chars()
        .collect::<Vec<_>>()
        .chunks(cols)
        .map(|chunk| SharedString::from(chunk.iter().collect::<String>()))
        .collect()
}

/// Build a chip's full-invocation block — the complete tool call the header
/// truncates to one line: the whole command, pattern, or URL, todo items one
/// per line, MCP/unknown input as pretty-printed JSON. Reuses the output
/// code-block payload so rendering and height stay one implementation.
pub fn call_block(call: &ToolCall) -> Option<ToolDetail> {
    let text: String = match call {
        ToolCall::Exec { command } => command.clone(),
        ToolCall::ReadFile { path } => path.clone(),
        ToolCall::WriteFile { path, content } => match content {
            Some(content) => format!("{path}\n{content}"),
            None => path.clone(),
        },
        ToolCall::EditFile { path, .. } => path.clone(),
        ToolCall::ApplyPatch { path } => path.clone().unwrap_or_else(|| "workspace".into()),
        ToolCall::Search { pattern, path } => match path {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern.clone(),
        },
        ToolCall::Glob { pattern } => pattern.clone(),
        ToolCall::WebFetch { url, prompt } => match prompt {
            Some(prompt) => format!("{url}\n{prompt}"),
            None => url.clone(),
        },
        ToolCall::WebSearch { query } => query.clone(),
        ToolCall::Todo { items } => items
            .iter()
            .map(|i| {
                let mark = match i.status() {
                    slopty_view_proto::TodoStatus::Completed => "[x]",
                    slopty_view_proto::TodoStatus::InProgress => "[~]",
                    slopty_view_proto::TodoStatus::Pending => "[ ]",
                };
                format!("{mark} {}", i.text)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        ToolCall::Mcp {
            server,
            tool,
            input,
        } => match input {
            Some(input) => format!(
                "{server} · {tool}\n{}",
                serde_json::to_string_pretty(input).unwrap_or_default()
            ),
            None => format!("{server} · {tool}"),
        },
        ToolCall::Unknown { name, input } => match input {
            Some(input) => format!(
                "{name}\n{}",
                serde_json::to_string_pretty(input).unwrap_or_default()
            ),
            None => name.clone(),
        },
    };
    let mut lines: Vec<SharedString> = text
        .lines()
        .flat_map(|l| wrap_cols(l, CALL_WRAP_COLS))
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let truncated_by = lines.len().saturating_sub(OUTPUT_DETAIL_MAX_LINES);
    lines.truncate(OUTPUT_DETAIL_MAX_LINES);
    Some(ToolDetail::Output {
        lines,
        truncated_by,
        stderr_from: None,
    })
}

/// Reduce an inline [`slopty_view_proto::ToolDiff`] to the changes pane's
/// [`crate::changes::FileDiff`]: hunks grouped with 3 context lines, dual
/// 1-based line numbers, unified-diff hunk headers, and add/del counts.
pub fn diff_to_file(diff: &slopty_view_proto::ToolDiff) -> crate::changes::FileDiff {
    use crate::changes::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    let old = diff.old_text.as_deref().unwrap_or("");
    let text_diff = similar::TextDiff::from_lines(old, &diff.new_text);
    let mut hunks = Vec::new();
    let (mut additions, mut deletions) = (0u32, 0u32);
    let mut max_line = 0u32;
    for group in text_diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        let header = format!(
            "@@ -{},{} +{},{} @@",
            old_range.start + 1,
            old_range.len(),
            new_range.start + 1,
            new_range.len(),
        );
        let mut lines = Vec::new();
        for op in &group {
            for change in text_diff.iter_changes(op) {
                let kind = match change.tag() {
                    similar::ChangeTag::Delete => {
                        deletions += 1;
                        LineKind::Del
                    }
                    similar::ChangeTag::Insert => {
                        additions += 1;
                        LineKind::Add
                    }
                    similar::ChangeTag::Equal => LineKind::Context,
                };
                let old_no = change.old_index().map(|n| n as u32 + 1);
                let new_no = change.new_index().map(|n| n as u32 + 1);
                max_line = max_line.max(old_no.unwrap_or(0)).max(new_no.unwrap_or(0));
                lines.push(DiffLine {
                    kind,
                    old_no,
                    new_no,
                    text: change.value().trim_end_matches('\n').to_owned(),
                });
            }
        }
        hunks.push(Hunk { header, lines });
    }
    FileDiff {
        path: diff.path.clone(),
        old_path: None,
        status: if diff.old_text.is_none() {
            FileStatus::Added
        } else {
            FileStatus::Modified
        },
        binary: false,
        notices: Vec::new(),
        hunks,
        additions,
        deletions,
        max_line,
    }
}

/// slopty: how a picture a tool returned is named (`slopty_ui::view_model::transcript::image_path`):
/// a file read names its file, unlike a picture the agent generated.
const TOOL_IMAGE_SCHEME: &str = "slopty-image:";

/// slopty: what a picture in the chat is called: one a tool returned by its name ("Image:
/// plot.png"), one the agent generated as such.
fn image_label(path: &str, name: &str) -> SharedString {
    if path.starts_with(TOOL_IMAGE_SCHEME) && !name.trim().is_empty() {
        format!("Image: {name}").into()
    } else {
        "Preview generated image".into()
    }
}

#[derive(Clone)]
pub enum RowKind {
    GeneratedImage {
        owner: String,
        path: String,
        name: String,
        mime_type: String,
    },
    /// An agent's `show_visual` script, rendered inline by [`crate::visuals`].
    Visual {
        owner: String,
        path: String,
        title: String,
    },
    User {
        /// Visible prompt (attachment-ref trailer already stripped).
        text: SharedString,
        /// `text` parsed as Markdown, when it uses any. `None` shows `text`
        /// as typed.
        markdown: Option<UserMarkdown>,
        /// Image refs parsed out of the message text (message-attachments.ts):
        /// thumbnails load from the owning device via ReadAttachmentChunk.
        attachments: Arc<Vec<crate::attachments::UserImageAttachment>>,
        /// Optimistic echo not yet confirmed by a doc frame.
        pending: bool,
    },
    /// One top-level markdown block of a completed message.
    Markdown {
        tree: Arc<BlockTree>,
        block_ix: usize,
    },
    /// One top-level block of a STREAMING message. Split per block like
    /// completed rows (only the tail blocks' versions change per commit, so
    /// the settled prefix is never respliced or re-rendered); rendered with
    /// the fade veil.
    LiveMarkdown {
        tree: Arc<BlockTree>,
        block_ix: usize,
    },
    /// slopty: a run of calls and thoughts, folded to one line until the person opens it;
    /// nothing opens it by itself (`docs/decisions/inspector.md`, "One fold depth").
    ToolGroup {
        summary: Sentence,
        tools: Arc<Vec<ToolItem>>,
    },
    InputChip {
        /// slopty: what kind of ask it was: "Question", "Approval", "Confirm", "Form", "Plan".
        label: SharedString,
        /// What the chip shows beside its label: a question's header, an approval's command
        /// (chat-view.tsx `InputChip`). slopty: an open ask's label says what it waits for
        /// ("Waiting for your answer"), which stays TRUE even across a run death: the composer
        /// keeps the panel up until the user answers, and the engine delivers a dead run's
        /// answer as a resumed turn.
        header: SharedString,
        resolved: bool,
    },
    ErrorChip {
        message: SharedString,
    },
    /// The fork seam: a labeled divider between copied history and the
    /// chat's own turns.
    ForkMarker {
        source_chat_id: SharedString,
        source_title: SharedString,
    },
    // slopty: Slopty's own rows (`transcript/extras.rs`): an error that says what the agent
    // said and can be tried again, a quiet divider, a plan's card, what a turn changed.
    /// A Claude Code too old to drive: the machine's version and the least one Slopty drives.
    UpdateNeeded {
        least: SharedString,
        have: SharedString,
    },
    ErrorCard {
        message: SharedString,
        detail: Option<SharedString>,
    },
    Marker {
        text: SharedString,
    },
    PlanHeader {
        copy: SharedString,
    },
    PlanFold {
        fold: SharedString,
        hidden_lines: usize,
    },
    TurnChanges {
        files: Arc<Vec<extras::TurnFile>>,
    },
    // slopty: a background task's notice and a quiet notice of the agent's (`transcript/work.rs`),
    // and a page shown inline (`crate::page`).
    TaskNote {
        task: Arc<slopty_view_doc::TaskRow>,
    },
    Notice {
        note: Arc<slopty_view_doc::NoteRow>,
    },
    Page {
        owner: String,
        card: Arc<slopty_view_doc::PageCard>,
    },
}

fn generated_image_devices(owner: &str, fallback: &[String]) -> Vec<String> {
    let mut devices = Vec::new();
    for device in std::iter::once(owner).chain(fallback.iter().map(String::as_str)) {
        if !device.is_empty() && !devices.iter().any(|id| id == device) {
            devices.push(device.to_owned());
        }
    }
    devices
}

/// A transcript row: stable id + content version (diff key) + block payload.
#[derive(Clone)]
pub struct Row {
    pub id: SharedString,
    pub version: u64,
    /// First row of its message entry (gets the turn gap).
    pub turn_start: bool,
    pub kind: RowKind,
    /// The owning message entry — hover anywhere on the entry's rows reveals
    /// its timestamp strip (slopty chat-view.tsx `group`/`group-hover`).
    pub entry_id: SharedString,
    /// Epoch-ms for the 16px hover-timestamp strip UNDER this row: set on the
    /// LAST row of a completed entry (user rows always; assistant rows only
    /// once streaming ends — "the turn isn't at a time yet", chat-view.tsx).
    pub timestamp: Option<i64>,
    /// Text copied by the entry-level hover action. Present only on the last
    /// settled row, beside the timestamp; tools and transport-only metadata
    /// are deliberately excluded.
    pub copy_text: Option<SharedString>,
    /// slopty: hidden while the named fold is closed (a long plan's rows past its first lines).
    pub compact_fold: Option<SharedString>,
}

/// slopty: how a time of day reads everywhere, as the system's clock is set: "%-I:%M %p"
/// ("11:12 PM") on a 12-hour clock, "%H:%M" ("23:12") on a 24-hour one. Read once.
pub fn clock_format() -> &'static str {
    static TWELVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *TWELVE.get_or_init(system_twelve_hour) { "%-I:%M %p" } else { "%H:%M" }
}

/// Whether the system's clock is a 12-hour one: the hour of the locale's "j" template (the
/// hour as the person's settings say it, System Settings' 24-hour switch included) has a day
/// period.
#[cfg(target_os = "macos")]
fn system_twelve_hour() -> bool {
    use objc2_foundation::{NSDateFormatter, NSLocale, ns_string};
    let locale = NSLocale::currentLocale();
    NSDateFormatter::dateFormatFromTemplate_options_locale(ns_string!("j"), 0, Some(&locale))
        .is_none_or(|format| format.to_string().contains('a'))
}

#[cfg(not(target_os = "macos"))]
fn system_twelve_hour() -> bool {
    true
}

/// slopty: the hover strip's time, as near as it is to `now_ms` (#17): the time alone today,
/// "Yesterday 3:45 PM", "Jul 1, 3:45 PM" this year, "Jul 1, 2025" before. On the system's clock;
/// pure over an explicit timezone so tests don't depend on the host's local time.
pub fn format_timestamp_near<Tz: chrono::TimeZone>(ms: i64, now_ms: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    use chrono::Datelike as _;
    let (Some(at), Some(now)) = (
        chrono::DateTime::from_timestamp_millis(ms),
        chrono::DateTime::from_timestamp_millis(now_ms),
    ) else {
        return String::new();
    };
    let (at, now) = (at.with_timezone(tz), now.with_timezone(tz));
    let (day, today) = (at.date_naive(), now.date_naive());
    if day == today {
        at.format(clock_format()).to_string()
    } else if today.pred_opt() == Some(day) {
        format!("Yesterday {}", at.format(clock_format()))
    } else if day.year() == today.year() {
        at.format(&format!("%b %-d, {}", clock_format())).to_string()
    } else {
        at.format("%b %-d, %Y").to_string()
    }
}

/// slopty: the hover strip time's whole moment, for its tooltip: "Wednesday, Jul 1, 2026,
/// 3:45 PM" (#17).
pub fn format_full_timestamp<Tz: chrono::TimeZone>(ms: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(utc) => utc
            .with_timezone(tz)
            .format(&format!("%A, %b %-d, %Y, {}", clock_format()))
            .to_string(),
        None => String::new(),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1_0000_01b3);
    }
    hash
}

fn tool_fingerprint(tools: &[ToolItem]) -> u64 {
    let mut acc = Vec::with_capacity(tools.len() * 8 + 1);
    for t in tools {
        acc.extend_from_slice(t.part_id.as_bytes());
        acc.push(0);
        let (label, detail) = tool_chip_content(&t.call);
        acc.extend_from_slice(label.as_bytes());
        acc.extend_from_slice(&(detail.len() as u32).to_le_bytes());
        acc.push(t.is_error as u8 | (t.resolved as u8) << 1);
        // Thought chips use `ToolCall::Unknown` — the kind is the
        // label/icon discriminator, so a flip must re-splice.
        acc.push(t.kind as u8);
        acc.push(t.images.len() as u8);
        // Detail payload arriving (or growing) must re-splice the row even
        // when the resolved bit didn't change.
        match t.detail.as_deref() {
            None => acc.push(0),
            Some(ToolDetail::Output {
                lines,
                truncated_by,
                stderr_from,
            }) => {
                acc.push(1);
                acc.extend_from_slice(&(stderr_from.unwrap_or(usize::MAX) as u64).to_le_bytes());
                acc.extend_from_slice(&(lines.len() as u32).to_le_bytes());
                acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
                let bytes: usize = lines.iter().map(|l| l.len()).sum();
                acc.extend_from_slice(&(bytes as u32).to_le_bytes());
            }
            Some(ToolDetail::Thought {
                lines,
                truncated_by,
            }) => {
                // Byte-exact plus style bits: a live mend can restyle runs
                // without changing the flattened length, and the row must
                // still re-splice.
                acc.push(4);
                acc.extend_from_slice(&(lines.len() as u32).to_le_bytes());
                acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
                for line in lines {
                    for run in line {
                        acc.extend_from_slice(run.text.as_bytes());
                        acc.push(
                            run.style.bold as u8
                                | (run.style.italic as u8) << 1
                                | (run.style.code as u8) << 2
                                | (run.style.strikethrough as u8) << 3
                                | (run.style.link.is_some() as u8) << 4
                                | (run.style.math as u8) << 5,
                        );
                    }
                    acc.push(b'\n');
                }
            }
            Some(ToolDetail::Diff { file, .. }) => {
                acc.push(2);
                acc.extend_from_slice(file.path.as_bytes());
                acc.extend_from_slice(&file.additions.to_le_bytes());
                acc.extend_from_slice(&file.deletions.to_le_bytes());
                acc.extend_from_slice(&(file.hunks.len() as u32).to_le_bytes());
            }
            Some(ToolDetail::Stats { stats }) => {
                acc.push(3);
                for stat in stats.iter() {
                    acc.extend_from_slice(stat.path.as_bytes());
                    acc.extend_from_slice(&stat.additions.to_le_bytes());
                    acc.extend_from_slice(&stat.deletions.to_le_bytes());
                }
            }
        }
        // The invocation block is pure over `call`, which the one-line hash
        // above only covers by length — hash its bytes so an in-place call
        // update (a streaming MCP input, a growing todo list) re-splices.
        if let Some(ToolDetail::Output {
            lines,
            truncated_by,
            ..
        }) = t.invocation.as_deref()
        {
            for line in lines {
                acc.extend_from_slice(line.as_bytes());
            }
            acc.extend_from_slice(&(*truncated_by as u32).to_le_bytes());
        }
        // Sidecar refs arriving after the resolve tick must re-splice too —
        // they add the fetch affordance without changing the detail payload.
        acc.push(t.output_ref.is_some() as u8 | (t.diff_ref.is_some() as u8) << 1);
        // Subagent lifecycle mutates the chip in place (status flips, the
        // live tail grows) — hash it so the row re-splices on every change.
        acc.push(
            t.subagent_ref.is_some() as u8
                | match t.subagent_status {
                    None => 0,
                    Some(SubagentStatus::Running) => 1 << 1,
                    Some(SubagentStatus::Done) => 2 << 1,
                    Some(SubagentStatus::Failed) => 3 << 1,
                },
        );
        if let Some(tail) = &t.subagent_tail {
            acc.extend_from_slice(tail.as_bytes());
        }
        // slopty: facts change in place (a timer, a step, a result line).
        if let Some(facts) = &t.facts {
            work::hash_into(&mut acc, facts.as_ref());
        }
    }
    fnv1a(&acc)
}

/// Clipboard payload for an assistant/system entry: authored text parts in
/// document order, preserving Markdown while excluding tool traces and other
/// structured parts.
fn assistant_copy_text(entry: &SessionMessageEntry) -> Option<SharedString> {
    let text = entry
        .parts
        .iter()
        .filter_map(|part| match part {
            // Inspect the trimmed view only to reject empty parts. Copy the
            // original bytes so indentation-based code blocks and Markdown
            // hard-break whitespace survive the clipboard round trip.
            MessagePart::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then(|| text.into())
}

/// Conservative first-frame fallback for whether a prompt may need a fold
/// affordance. Once the text element has measured, `render_user_body` replaces
/// this proxy with the exact wrapped-line count, so glyph width and script no
/// longer affect eligibility heuristically.
pub fn user_message_needs_collapse(text: &str) -> bool {
    text.lines().count() > USER_COLLAPSED_LINES || text.chars().count() > USER_COLLAPSE_CHARS
}

/// Layout transitions need more time when they travel farther, otherwise a
/// 1,000px pasted log crosses the screen in the same 200ms as a six-line note
/// and reads as a snap. Keep ordinary messages close to the catalog RESIZE
/// timing, then scale to an 850ms ceiling for genuinely large pasted content.
pub fn user_resize_duration_ms(height_delta: f32) -> u64 {
    (220.0 + height_delta.max(0.0) * 0.32).min(850.0).round() as u64
}

/// Short folds keep the app's familiar decisive ease-out. Large folds use
/// ease-in-out so thousands of pixels do not disappear in the first few frames
/// of an aggressively front-loaded curve.
pub fn user_resize_spec(height_delta: f32) -> motion::MotionSpec {
    let curve = if height_delta > 500.0 {
        motion::EASE_IN_OUT
    } else {
        motion::EASE_OUT
    };
    motion::MotionSpec::new(user_resize_duration_ms(height_delta), curve)
}

#[cfg(test)]
thread_local! { static FORBID_ROW_PREPARATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// Build the block rows of one (already continuation-joined) entry.
///
/// `parse` maps `(part_key, text)` to a block tree — the entity supplies
/// incremental parsers for live parts and a cache for complete ones; tests pass
/// a plain `parse_full`.
/// slopty: a turn's rows are its records in the order they were written, never re-sorted
/// (`docs/decisions/inspector.md`, "Rows in record order"): its narration where it was said, each
/// run of calls and thoughts one group line, each notice and error where it happened. A late
/// record lands in its place and no row ever changes container, so the turn's end moves nothing.
pub fn rows_for_entry(
    entry: &SessionMessageEntry,
    pending: bool,
    parse: &mut dyn FnMut(&str, &str) -> Arc<BlockTree>,
) -> Vec<Row> {
    #[cfg(test)]
    FORBID_ROW_PREPARATION
        .with(|forbidden| assert!(!forbidden.get(), "row preparation ran on the UI thread"));
    let mut rows: Vec<Row> = Vec::new();
    let streaming = entry.status == Some(MessageStatus::Streaming);
    let entry_id: SharedString = entry.id.clone().into();

    if entry.role == MessageRole::User {
        let raw: String = entry
            .parts
            .iter()
            .filter_map(|p| match p {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        // Attachment refs ride the plain text (the `withAttachments`
        // transport); split them back out for the thumbnail strip.
        let parsed = crate::attachments::parse_user_message_images(&raw);
        // slopty: the words as the agent's record has them: a TUI's prompt holds no chips.
        let text = agent_message_display(&parsed.text);
        // slopty: a TUI's prompt holds no chips, so any prompt may render its Markdown.
        let markdown = Some(crate::markdown::parse_message(&text))
            .filter(|tree| !plain_markdown(tree))
            .map(|tree| UserMarkdown {
                folded: folded_markdown(&tree, USER_COLLAPSED_LINES + USER_FOLD_OVERDRAW_LINES)
                    .map(Arc::new),
                full: Arc::new(tree),
            });
        let attachments = parsed.attachments;
        let copy_text = (!text.trim().is_empty()).then(|| SharedString::from(text.clone()));
        return vec![Row {
            id: entry.id.clone().into(),
            version: (raw.len() as u64) << 1 | pending as u64,
            turn_start: true,
            kind: RowKind::User {
                text: text.into(),
                markdown,
                attachments: Arc::new(attachments),
                pending,
            },
            entry_id,
            // User rows always carry the strip (chat-view.tsx: whenever
            // `createdAt` exists — the optimistic echo included).
            timestamp: Some(entry.created_at),
            copy_text,
            compact_fold: None,
        }];
    }

    // Assistant/system: split parts into block rows, folding consecutive
    // ordinary tools. Agent/spawn chips flush into their own group so they
    // never share a collapse with Reads/Runs.
    let last_part_ix = entry.parts.len().saturating_sub(1);
    let mut pending_group: Vec<ToolItem> = Vec::new();
    let mut group_last_part_ix = 0usize;

    // slopty: a group is keyed by its first step's record id, so a group that grows, or that a
    // spawn splits as its input lands, keeps its key and its fold (`inspector.md`, "Keys").
    let flush_group = |rows: &mut Vec<Row>, group: &mut Vec<ToolItem>, last_ix: usize| {
        let Some(first) = group.first() else {
            return;
        };
        let id: SharedString = format!("{}#g:{}", entry.id, first.part_id).into();
        let tools = std::mem::take(group);
        let waiting = waiting_on(entry.parts.get(last_ix + 1));
        rows.push(Row {
            id,
            version: tool_fingerprint(&tools)
                ^ (u64::from(waiting.is_some()) << 63)
                ^ (u64::from(streaming) << 61),
            turn_start: false,
            kind: RowKind::ToolGroup {
                summary: tool_group_summary_waiting(&tools, waiting, !streaming),
                tools: Arc::new(tools),
            },
            entry_id: entry.id.clone().into(),
            timestamp: None,
            copy_text: None,
            compact_fold: None,
        });
    };

    for (part_ix, part) in entry.parts.iter().enumerate() {
        match part {
            MessagePart::Tool {
                call,
                is_error,
                resolved,
                output,
                diff,
                output_ref,
                output_bytes,
                diff_ref,
                diff_stats,
                subagent_ref,
                subagent_status,
                subagent_tail,
                facts,
                ..
            } => {
                let item = ToolItem {
                    part_id: part.id().to_owned(),
                    call: call.clone(),
                    is_error: *is_error,
                    resolved: *resolved,
                    detail: with_stderr(
                        tool_detail(output.as_deref(), diff.as_ref(), diff_stats.as_deref()),
                        facts.as_deref().and_then(|facts| facts.stderr.as_deref()),
                    )
                    .map(Arc::new),
                    invocation: call_block(call).map(Arc::new),
                    output_ref: output_ref.clone().map(SharedString::from),
                    output_bytes: *output_bytes,
                    diff_ref: diff_ref.clone().map(SharedString::from),
                    subagent_ref: subagent_ref.clone().map(SharedString::from),
                    subagent_status: *subagent_status,
                    subagent_tail: subagent_tail.clone().map(SharedString::from),
                    kind: ToolItemKind::Call,
                    images: crate::tool_images::image_paths(call, output.as_deref()),
                    facts: facts.as_deref().cloned().map(Arc::new), // slopty: its facts.
                };
                if pending_group
                    .first()
                    .is_some_and(|head| is_agent_tool(head) != is_agent_tool(&item))
                {
                    // Agent chips don't share a fold with ordinary tools:
                    // flush whenever the genus flips so each group is uniform.
                    flush_group(&mut rows, &mut pending_group, group_last_part_ix);
                }
                pending_group.push(item);
                group_last_part_ix = part_ix;
            }
            // Thinking rides the SAME accordion as the tools around it
            // (user request) — a thought chip in the group, not its own row.
            MessagePart::Reasoning { id: part_id, text, thought_ms } => {
                if text.trim().is_empty() {
                    continue;
                }
                // Live only while it is the tail of a streaming reply — once
                // text or a tool follows, the thought is finished even though
                // the entry still streams.
                let live = streaming && part_ix == last_part_ix;
                // The same parse wiring as text parts: incremental while
                // streaming, hanging inline markers mended for display, the
                // settled cache once complete.
                let tree = parse(&format!("{}#{}", entry.id, part_id), text);
                let mut item = thought_item(part_id, &tree, live);
                // slopty: how long it thought, for the group's line.
                if let Some(ms) = *thought_ms {
                    item.facts = Some(Arc::new(slopty_view_doc::ToolFacts {
                        thought_ms: Some(ms),
                        ..slopty_view_doc::ToolFacts::default()
                    }));
                }
                if pending_group.first().is_some_and(is_agent_tool) {
                    // Thoughts join ordinary tool groups; agent (spawn-link)
                    // groups stay pure, exactly like the tool genus rule.
                    flush_group(&mut rows, &mut pending_group, group_last_part_ix);
                }
                pending_group.push(item);
                group_last_part_ix = part_ix;
            }
            other => {
                flush_group(&mut rows, &mut pending_group, group_last_part_ix);
                match other {
                    MessagePart::Text { id: part_id, text } => {
                        if text.trim().is_empty() {
                            continue;
                        }
                        let key = format!("{}#{}", entry.id, part_id);
                        let tree = parse(&key, text);
                        // slopty: a proposed plan is a card, folded past its first lines.
                        if extras::is_plan_part(part_id) {
                            extras::plan_rows(&mut rows, entry, &key, text, &tree, streaming);
                            continue;
                        }
                        // Live and completed parts split identically — one row
                        // per top-level block, same ids, so the live→complete
                        // handoff never changes row identity. The version is a
                        // content hash of the block's bytes (LSB = streaming),
                        // so a commit only splices rows whose bytes actually
                        // changed — the settled prefix of a live reply is
                        // untouched (and its render caches stay valid).
                        for block_ix in 0..tree.blocks.len() {
                            let range = &tree.blocks[block_ix].range;
                            let end = range.end.min(text.len());
                            let bytes = text
                                .as_bytes()
                                .get(range.start.min(end)..end)
                                .unwrap_or_default();
                            let version = (fnv1a(bytes) << 1) | streaming as u64;
                            rows.push(Row {
                                id: format!("{key}.{block_ix}").into(),
                                version,
                                turn_start: false,
                                entry_id: entry_id.clone(),
                                timestamp: None,
                                copy_text: None,
                                compact_fold: None,
                                kind: if streaming {
                                    RowKind::LiveMarkdown {
                                        tree: tree.clone(),
                                        block_ix,
                                    }
                                } else {
                                    RowKind::Markdown {
                                        tree: tree.clone(),
                                        block_ix,
                                    }
                                },
                            });
                        }
                    }
                    MessagePart::Image {
                        id: part_id,
                        path,
                        name,
                        mime_type,
                    } => {
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: fnv1a(
                                format!("{}\0{path}\0{name}\0{mime_type}", entry.device_id)
                                    .as_bytes(),
                            ),
                            turn_start: false,
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                            compact_fold: None,
                            kind: RowKind::GeneratedImage {
                                owner: entry.device_id.clone(),
                                path: path.clone(),
                                name: name.clone(),
                                mime_type: mime_type.clone(),
                            },
                        });
                    }
                    MessagePart::Visual {
                        id: part_id,
                        path,
                        title,
                    } => {
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: fnv1a(
                                format!("{}\0{path}\0{title}", entry.device_id).as_bytes(),
                            ),
                            turn_start: false,
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                            compact_fold: None,
                            kind: RowKind::Visual {
                                owner: entry.device_id.clone(),
                                path: path.clone(),
                                title: title.clone(),
                            },
                        });
                    }
                    MessagePart::Input {
                        id: part_id,
                        questions,
                        resolved,
                        ..
                    } => {
                        // slopty: the chip names what was asked by its kind, and shows the ask.
                        let (label, header) = input_chip_words(questions);
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: fnv1a(format!("{label}\0{header}").as_bytes()) << 1
                                | *resolved as u64,
                            turn_start: false,
                            kind: RowKind::InputChip {
                                label,
                                header,
                                resolved: *resolved,
                            },
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                            compact_fold: None,
                        });
                    }
                    // slopty: a Claude Code too old to drive is a banner naming its machine.
                    MessagePart::Error {
                        id: part_id,
                        message,
                        detail,
                        ..
                    } if extras::too_old_of(message, detail.as_deref()).is_some() => {
                        if let Some(row) = extras::update_needed_row(&entry.id, part_id, message, detail.as_deref()) {
                            rows.push(row);
                        }
                    }
                    // slopty: an error with the agent's own words is a card.
                    MessagePart::Error {
                        id: part_id,
                        message,
                        detail: Some(detail),
                    } => {
                        rows.push(extras::error_card_row(&entry.id, part_id, message, Some(detail)));
                    }
                    MessagePart::Error {
                        id: part_id,
                        message,
                        ..
                    } => {
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: message.len() as u64,
                            turn_start: false,
                            kind: RowKind::ErrorChip {
                                // Harness-generated; the chip is one line.
                                message: single_line(message).into(),
                            },
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                            compact_fold: None,
                        });
                    }
                    MessagePart::Fork {
                        id: part_id,
                        source_chat_id,
                        source_title,
                    } => {
                        rows.push(Row {
                            id: format!("{}#{}", entry.id, part_id).into(),
                            version: fnv1a(source_title.as_bytes()),
                            turn_start: false,
                            kind: RowKind::ForkMarker {
                                source_chat_id: source_chat_id.clone().into(),
                                source_title: single_line(source_title).into(),
                            },
                            entry_id: entry_id.clone(),
                            timestamp: None,
                            copy_text: None,
                            compact_fold: None,
                        });
                    }
                    // slopty: a quiet divider.
                    MessagePart::Marker { id: part_id, text } => {
                        rows.push(extras::marker_row(&entry.id, part_id, text));
                    }
                    // slopty: a task's notice, a quiet notice, a page shown inline.
                    MessagePart::Task { id: part_id, task } => {
                        rows.push(work::task_row(&entry.id, part_id, task));
                    }
                    MessagePart::Note { id: part_id, note } => {
                        rows.push(work::notice_row(&entry.id, part_id, note));
                    }
                    MessagePart::Page { id: part_id, card } => {
                        rows.push(crate::page::page_row(entry, part_id, card));
                    }
                    // Tools and thoughts are grouped by the outer arms;
                    // nothing reaches here.
                    MessagePart::Tool { .. } | MessagePart::Reasoning { .. } => {}
                }
            }
        }
    }
    flush_group(&mut rows, &mut pending_group, group_last_part_ix);

    // slopty: a settled turn says what it changed, above its done line, which ends it (#4).
    let done = rows.last().is_some_and(is_done_line);
    if !streaming && let Some(row) = extras::turn_changes_row(entry) {
        let at = if done { rows.len() - 1 } else { rows.len() };
        rows.insert(at, row);
    }
    if let Some(first) = rows.first_mut() {
        first.turn_start = true;
    }
    // Timestamp strip under the entry's LAST row once the turn has settled
    // (chat-view.tsx: "No timestamp hover mid-stream"). The version bit keeps
    // the diff key honest for last-row kinds whose own version wouldn't
    // change when streaming flips off (chips).
    if !streaming
        && let Some(last) = rows.last_mut()
        && !matches!(last.kind, RowKind::ForkMarker { .. })
    {
        // slopty: a done line says when the turn ended; the strip under it does not say it
        // again (#4).
        last.timestamp = Some(if done { 0 } else { entry.created_at });
        last.copy_text = assistant_copy_text(entry);
        last.version ^= 1 << 62;
    }
    rows
}

/// slopty: whether `row` is a turn's done line ("Worked for 4s · done 9:00 AM").
fn is_done_line(row: &Row) -> bool {
    matches!(&row.kind, RowKind::Notice { note } if note.kind == DONE_NOTE)
}

/// slopty: the kind of a turn's done line (`slopty_ui::view_model::folds::DONE`).
const DONE_NOTE: &str = "done";

/// slopty: what a request right after a group holds its open call up for (#3).
fn waiting_on(next: Option<&MessagePart>) -> Option<&'static str> {
    match next {
        Some(MessagePart::Input { resolved: false, questions, .. }) => {
            Some(match questions.first().map(|q| q.kind.as_str()) {
                Some("approval" | "retry") => "approval",
                Some("plan") => "your review",
                _ => "your answer",
            })
        }
        _ => None,
    }
}

/// `SLOPTY_FRAME_STATS=1` logs live-row render-cost percentiles (p50/p95 µs
/// over rolling windows of [`FRAME_STATS_WINDOW`] samples) at `warn` level —
/// the smoothness measurement knob. Off by default; zero cost when off.
fn frame_stats_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var("SLOPTY_FRAME_STATS").is_ok_and(|v| !v.is_empty() && v != "0"))
}

const FRAME_STATS_WINDOW: usize = 240;

/// Opt-in cadence counters complement per-row timings: inexpensive rows can
/// still exhaust a battery when an unrelated animation rebuilds them at 120Hz.
pub(crate) fn record_view_frame(view: &'static str) -> bool {
    if !frame_stats_enabled() {
        return false;
    }
    thread_local! {
        static COUNTERS: RefCell<HashMap<&'static str, (Instant, u64)>> = RefCell::default();
    }
    COUNTERS.with(|counters| {
        let mut counters = counters.borrow_mut();
        let (start, frames) = counters.entry(view).or_insert_with(|| (crate::clock::stopwatch(), 0));
        *frames += 1;
        let elapsed = crate::clock::spent(*start).as_secs_f64();
        if elapsed >= 1.0 {
            tracing::warn!(
                view,
                frames_per_second = *frames as f64 / elapsed,
                "view render cadence"
            );
            *start = crate::clock::stopwatch();
            *frames = 0;
            return true;
        }
        false
    })
}

/// `SLOPTY_NO_RENDER_CACHE=1` bypasses the cross-frame flatten cache — the
/// A/B knob for the frame-cost measurement above.
fn render_cache_disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| {
        std::env::var("SLOPTY_NO_RENDER_CACHE").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

fn record_live_frame_us(us: u64) {
    thread_local! {
        static SAMPLES: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    }
    SAMPLES.with(|s| {
        let mut s = s.borrow_mut();
        s.push(us);
        if s.len() >= FRAME_STATS_WINDOW {
            s.sort_unstable();
            let p50 = s[s.len() / 2];
            let p95 = s[s.len() * 95 / 100];
            let max = *s.last().unwrap();
            tracing::warn!(
                n = s.len(),
                p50_us = p50,
                p95_us = p95,
                max_us = max,
                "live-row render cost"
            );
            s.clear();
        }
    });
}

/// How [`parse_for_row`] produced its tree — carries the incremental parser's
/// work counters so callers (and tests) can see that per-append parse work is
/// bounded by the reparsed tail, never the whole accumulated reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Streaming row: the live [`IncrementalParser`] advanced by one commit.
    Incremental {
        /// Bytes fed through `parse_full` for this commit (the reparse tail).
        parsed_bytes: usize,
        /// Leading top-level blocks left untouched (render caches stay valid).
        stable_prefix_blocks: usize,
    },
    /// Completed row served from the settled tree cache (no parse at all).
    Cached,
    /// Live→complete handoff: the live parser's exact tree was adopted.
    Handoff,
    /// Completed row parsed from scratch.
    Full,
}

/// The transcript's markdown parse wiring, extracted for testability: one call
/// per text part per sync. Streaming parts keep one [`IncrementalParser`] per
/// row key and advance it with the full accumulated text (`set_text` takes the
/// O(tail) append path for the prefix-extensions the doc watch delivers);
/// completed parts hit the settled cache, adopt the live parser's tree on the
/// live→complete flip (flicker-free handoff), or do one full parse.
pub fn parse_for_row(
    streaming: bool,
    key: &str,
    text: &str,
    live_parsers: &mut HashMap<String, IncrementalParser>,
    tree_cache: &mut HashMap<String, (usize, Arc<BlockTree>)>,
) -> (Arc<BlockTree>, ParseOutcome) {
    if streaming {
        let parser = live_parsers.entry(key.to_string()).or_default();
        parser.set_text(text);
        (
            // Display tree: hanging inline markers mended so closers arriving
            // later never reflow painted text (markdown/mend.rs). Completed
            // rows below use the canonical tree — the honest settle.
            Arc::new(parser.display_tree()),
            ParseOutcome::Incremental {
                parsed_bytes: parser.last_parse_bytes(),
                stable_prefix_blocks: parser.stable_prefix_blocks(),
            },
        )
    } else {
        if let Some((len, tree)) = tree_cache.get(key)
            && *len == text.len()
        {
            return (tree.clone(), ParseOutcome::Cached);
        }
        // On the live→complete flip reuse the live parser's tree when
        // the sources match — the split rows then share the exact tree
        // the unsplit row painted, guaranteeing a flicker-free handoff.
        let (tree, outcome) = match live_parsers.remove(key) {
            Some(parser) if parser.source() == text => {
                (Arc::new(parser.tree().clone()), ParseOutcome::Handoff)
            }
            _ => (Arc::new(parse_full(text)), ParseOutcome::Full),
        };
        tree_cache.insert(key.to_string(), (text.len(), tree.clone()));
        (tree, outcome)
    }
}

/// Markdown row ids are `{entry}#{part}.{blockIx}` — the part prefix is
/// everything before the block index.
fn part_prefix(id: &str) -> &str {
    id.rsplit_once('.').map(|(p, _)| p).unwrap_or(id)
}

/// Vertical gap opening `row` given its predecessor: turn gap at turn starts;
/// the markdown block gap between sibling block rows split from the same text
/// part — matching the live row's internal spacing exactly, so the
/// live→split handoff cannot shift a pixel. Tool groups get one larger global
/// step on either boundary so their dense chip stack has room to breathe.
pub fn top_gap_for(prev: Option<&Row>, row: &Row) -> f32 {
    if row.turn_start {
        // slopty: a prompt heads its turn: the big step comes before it, and its answer starts
        // one turn gap under it, with no hidden lane between them (#1).
        return if matches!(row.kind, RowKind::User { .. }) { PROMPT_GAP } else { Theme::SPACE_LG };
    }
    let is_md = |k: &RowKind| matches!(k, RowKind::Markdown { .. } | RowKind::LiveMarkdown { .. });
    let same_part_markdown = prev.is_some_and(|p| {
        is_md(&p.kind) && is_md(&row.kind) && part_prefix(&p.id) == part_prefix(&row.id)
    });
    if same_part_markdown {
        render::MD_BLOCK_GAP
    } else if matches!(row.kind, RowKind::ToolGroup { .. })
        || prev.is_some_and(|row| matches!(row.kind, RowKind::ToolGroup { .. }))
    {
        Theme::SPACE_MD
    } else {
        Theme::SPACE_SM
    }
}

/// Minimal splice for a row-set change: `Some((old_range, new_count))`, or
/// `None` when the sets are identical by (id, version).
pub fn diff_rows(old: &[Row], new: &[Row]) -> Option<(Range<usize>, usize)> {
    let eq = |a: &Row, b: &Row| a.id == b.id && a.version == b.version;
    let mut prefix = 0usize;
    let max_prefix = old.len().min(new.len());
    while prefix < max_prefix && eq(&old[prefix], &new[prefix]) {
        prefix += 1;
    }
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let mut suffix = 0usize;
    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    while suffix < max_suffix && eq(&old[old.len() - 1 - suffix], &new[new.len() - 1 - suffix]) {
        suffix += 1;
    }
    Some((prefix..old.len() - suffix, new.len() - suffix - prefix))
}

// ---------------------------------------------------------------------------
// Tool summaries / chips (pure)
// ---------------------------------------------------------------------------

/// slopty: a work group's summary as one sentence (Amp, `.research/amp-study-2026-10-19.md`
/// item 2): its clauses joined by ", ", or " and " between two, each followed by what it adds in
/// parentheses: "Ran 3 commands (1 failed), edited 2 files (+40 −12)". `marks` say which spans
/// are drawn apart: the joins and parentheses faint, an edit's counts as a diff's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sentence {
    pub text: SharedString,
    pub marks: Arc<[(Range<usize>, Mark)]>,
}

/// slopty: how a span of a [`Sentence`] is drawn apart from its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A join or a parenthesis, in faint ink.
    Join,
    /// Lines an edit added, in the diff's ink.
    Added,
    /// Lines an edit removed, in the diff's ink.
    Removed,
}

impl std::ops::Deref for Sentence {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl From<&str> for Sentence {
    fn from(text: &str) -> Self {
        Self { text: SharedString::from(text.to_owned()), marks: Arc::from([]) }
    }
}

impl std::fmt::Display for Sentence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// slopty: one clause of a [`Sentence`]: its words, then what it adds.
struct Clause {
    words: String,
    adds: Vec<Adds>,
}

/// slopty: what a clause adds in its parentheses.
enum Adds {
    Words(String),
    /// Lines added and removed.
    Lines(u64, u64),
}

impl Clause {
    fn new(words: impl Into<String>) -> Self {
        Self { words: words.into(), adds: Vec::new() }
    }

    fn failed(mut self, failed: usize) -> Self {
        if failed > 0 {
            self.adds.push(Adds::Words(format!("{failed} failed")));
        }
        self
    }
}

/// slopty: `clauses` as one sentence, its first letter lifted.
fn sentence(clauses: Vec<Clause>) -> Sentence {
    let mut text = String::new();
    let mut marks: Vec<(Range<usize>, Mark)> = Vec::new();
    let mut push = |text: &mut String, words: &str, mark: Option<Mark>| {
        let start = text.len();
        text.push_str(words);
        if let Some(mark) = mark {
            marks.push((start..text.len(), mark));
        }
    };
    let count = clauses.len();
    for (ix, clause) in clauses.into_iter().enumerate() {
        if ix > 0 {
            push(&mut text, if count == 2 { " and " } else { ", " }, Some(Mark::Join));
        }
        push(&mut text, &clause.words, None);
        if clause.adds.is_empty() {
            continue;
        }
        push(&mut text, " (", Some(Mark::Join));
        for (ax, adds) in clause.adds.iter().enumerate() {
            if ax > 0 {
                push(&mut text, ", ", Some(Mark::Join));
            }
            match adds {
                Adds::Words(words) => push(&mut text, words, None),
                Adds::Lines(added, removed) => {
                    push(&mut text, &format!("+{added}"), Some(Mark::Added));
                    push(&mut text, " ", None);
                    push(&mut text, &format!("−{removed}"), Some(Mark::Removed));
                }
            }
        }
        push(&mut text, ")", Some(Mark::Join));
    }
    if let Some(first) = text.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Sentence { text: text.into(), marks: marks.into() }
}

/// The ToolGroup summary line — "Ran 3 commands, edited 2 files".
pub fn tool_group_summary(tools: &[ToolItem]) -> String {
    tool_group_summary_waiting(tools, None, false).text.to_string()
}

/// slopty: [`tool_group_summary`] in the tense of each call (#3): what settled is said in the
/// past ("Read 3 files"), what is under way in its participle ("running 1 command"), and the
/// call a request holds up says what it waits for (`waiting`, "approval") and what it is
/// ("waiting for approval (run touch refused.txt)"). Once its turn is over (`turn_over`), a call
/// that never settled is no longer under way: it says it stopped ("1 command stopped").
pub fn tool_group_summary_waiting(tools: &[ToolItem], waiting: Option<&str>, turn_over: bool) -> Sentence {
    #[cfg(test)]
    FORBID_ROW_PREPARATION
        .with(|forbidden| assert!(!forbidden.get(), "tool summary formatting ran on UI thread"));
    // The call the request holds up: the group's last one still open.
    let held = waiting.and_then(|_| {
        tools
            .iter()
            .rposition(|t| t.kind == ToolItemKind::Call && !t.resolved && !t.is_error)
    });
    let live = |ix: usize, t: &ToolItem| {
        t.kind == ToolItemKind::Call && !t.resolved && !t.is_error && Some(ix) != held
    };
    let under_way: Vec<&ToolItem> =
        tools.iter().enumerate().filter(|(ix, t)| live(*ix, t)).map(|(_, t)| t).collect();
    let settled: Vec<&ToolItem> = tools
        .iter()
        .enumerate()
        .filter(|(ix, t)| !live(*ix, t) && Some(*ix) != held)
        .map(|(_, t)| t)
        .collect();
    let mut clauses = past_clauses(&settled);
    if turn_over {
        clauses.extend(stopped_clauses(&under_way));
    } else {
        clauses.extend(live_clauses(&under_way));
    }
    if let (Some(what), Some(ix)) = (waiting, held) {
        let mut name = chip_name(&tools[ix]).to_string();
        if let Some(first) = name.get_mut(..1) {
            first.make_ascii_lowercase();
        }
        let mut clause = Clause::new(format!("waiting for {what}"));
        clause.adds.push(Adds::Words(name));
        clauses.push(clause);
    }
    sentence(clauses)
}

/// slopty: the calls a turn ended on before they settled (an interrupted run, a record without
/// its results), said as stopped, never as still running (#12).
fn stopped_clauses(tools: &[&ToolItem]) -> Vec<Clause> {
    let commands = tools.iter().filter(|t| matches!(t.call, ToolCall::Exec { .. })).count();
    let other = tools.len() - commands;
    let mut clauses = Vec::new();
    match commands {
        0 => {}
        1 => clauses.push(Clause::new("1 command stopped")),
        n => clauses.push(Clause::new(format!("{n} commands stopped"))),
    }
    match other {
        0 => {}
        1 => clauses.push(Clause::new("1 call stopped")),
        n => clauses.push(Clause::new(format!("{n} calls stopped"))),
    }
    clauses
}

/// slopty: `n` of a noun, one or many.
fn counted(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// slopty: what a call is, for the clause that counts it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CallKind {
    Command,
    Listing,
    Edit,
    Read,
    Search,
    Fetch,
    Todo,
    /// slopty: a subagent's run, said as the work it hands off, not as a tool called.
    Subagent,
    Other,
}

fn call_kind(call: &ToolCall) -> CallKind {
    match call {
        call if lists_directory(call) => CallKind::Listing,
        call if is_agent_call(call) => CallKind::Subagent,
        ToolCall::Exec { .. } => CallKind::Command,
        ToolCall::WriteFile { .. } | ToolCall::EditFile { .. } | ToolCall::ApplyPatch { .. } => CallKind::Edit,
        ToolCall::ReadFile { .. } => CallKind::Read,
        ToolCall::Search { .. } | ToolCall::Glob { .. } | ToolCall::WebSearch { .. } => CallKind::Search,
        ToolCall::WebFetch { .. } => CallKind::Fetch,
        ToolCall::Todo { .. } => CallKind::Todo,
        ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => CallKind::Other,
    }
}

/// slopty: the files `tools` edited, each once, in order.
fn edited_paths<'a>(tools: &[&'a ToolItem]) -> Vec<&'a str> {
    let mut edited: Vec<&str> = Vec::new();
    for t in tools {
        let path = match &t.call {
            ToolCall::WriteFile { path, .. } | ToolCall::EditFile { path, .. } => path.as_str(),
            ToolCall::ApplyPatch { path } => path.as_deref().unwrap_or("patch"),
            _ => continue,
        };
        if !edited.contains(&path) {
            edited.push(path);
        }
    }
    edited
}

/// slopty: what the calls under way are doing, in the TUI's live verbs (#3).
fn live_clauses(tools: &[&ToolItem]) -> Vec<Clause> {
    let of = |kind: CallKind| tools.iter().filter(|t| call_kind(&t.call) == kind).count();
    let edits: Vec<&ToolItem> = tools.iter().copied().filter(|t| call_kind(&t.call) == CallKind::Edit).collect();
    let editing = match edited_paths(&edits).as_slice() {
        [] => None,
        [path] => Some(format!("editing {}", file_badge_name(path))),
        many => Some(format!("editing {} files", many.len())),
    };
    let (commands, listings, reads, fetches, other) = (
        of(CallKind::Command),
        of(CallKind::Listing),
        of(CallKind::Read),
        of(CallKind::Fetch),
        of(CallKind::Other),
    );
    [
        (commands > 0).then(|| format!("running {}", counted(commands, "command", "commands"))),
        (listings > 0).then(|| format!("listing {}", counted(listings, "directory", "directories"))),
        editing,
        (reads > 0).then(|| format!("reading {}", counted(reads, "file", "files"))),
        (of(CallKind::Search) > 0).then(|| "searching".to_owned()),
        (fetches > 0).then(|| format!("fetching {}", counted(fetches, "page", "pages"))),
        (of(CallKind::Todo) > 0).then(|| "updating todos".to_owned()),
        (of(CallKind::Subagent) > 0)
            .then(|| format!("running {}", counted(of(CallKind::Subagent), "subagent", "subagents"))),
        (other > 0).then(|| format!("calling {}", counted(other, "tool", "tools"))),
    ]
    .into_iter()
    .flatten()
    .map(Clause::new)
    .collect()
}

/// The summary of calls that settled, and thoughts and notes, in past verbs. A failure is said
/// in the parentheses of the clause whose call it was ("ran 3 commands (1 failed)"), never loose
/// at the end.
fn past_clauses(tools: &[&ToolItem]) -> Vec<Clause> {
    let calls: Vec<&ToolItem> = tools.iter().copied().filter(|t| t.kind == ToolItemKind::Call).collect();
    // slopty: directory listings are counted apart from commands, as the TUI's group line
    // counts them ("listed 2 directories"). A listing that failed listed nothing: it counts as
    // the failed command it was.
    let kind = |t: &ToolItem| match call_kind(&t.call) {
        CallKind::Listing if t.is_error => CallKind::Command,
        kind => kind,
    };
    let of = |want: CallKind| calls.iter().copied().filter(|t| kind(t) == want).collect::<Vec<_>>();
    let failed = |of: &[&ToolItem]| of.iter().filter(|t| t.is_error && !denied(t)).count();
    let mut clauses: Vec<Clause> = Vec::new();
    // slopty: how long it thought, where each thought says, as the TUI's "Thought for 12s".
    let thoughts = tools.iter().filter(|t| t.kind == ToolItemKind::Thought).count();
    let thought_ms: Option<u64> = tools
        .iter()
        .filter(|t| t.kind == ToolItemKind::Thought)
        .map(|t| t.facts.as_ref().and_then(|facts| facts.thought_ms))
        .sum();
    match (thoughts, thought_ms) {
        (0, _) => {}
        (_, Some(ms)) if ms >= 1_000 => clauses.push(Clause::new(format!(
            "thought for {}",
            slopty_view_doc::work::duration_words(ms)
        ))),
        // slopty: a clause says what was done, as its siblings do ("Thought once and ran 1
        // command"); "Thought process" is a thought's own row, never a clause of a sentence.
        (_, Some(_)) => clauses.push(Clause::new("thought briefly")),
        (1, None) => clauses.push(Clause::new("thought once")),
        (n, None) => clauses.push(Clause::new(format!("thought {n} times"))),
    }
    // slopty: the agent's words between its calls are no work done ("wrote 8 notes" read as
    // files written): they read in place once the divider opens.
    let commands = of(CallKind::Command);
    if !commands.is_empty() {
        clauses.push(
            Clause::new(format!("ran {}", counted(commands.len(), "command", "commands")))
                .failed(failed(&commands)),
        );
    }
    let edits = of(CallKind::Edit);
    let edited = edited_paths(&edits);
    if !edited.is_empty() {
        // slopty: the lines its edits added and removed, the number a reviewer wants first.
        let (added, removed) = edits
            .iter()
            .filter_map(|t| t.facts.as_ref().and_then(|facts| facts.changed))
            .fold((0_u64, 0_u64), |(a, r), [add, rem]| (a.saturating_add(add), r.saturating_add(rem)));
        let mut clause = Clause::new(format!("edited {}", counted(edited.len(), "file", "files")));
        if added > 0 || removed > 0 {
            clause.adds.push(Adds::Lines(added, removed));
        }
        clauses.push(clause.failed(failed(&edits)));
    }
    for (want, verb, one, many) in [
        (CallKind::Read, "read", "file", "files"),
        (CallKind::Search, "searched", "time", "times"),
        (CallKind::Fetch, "fetched", "page", "pages"),
        (CallKind::Subagent, "delegated to", "subagent", "subagents"),
        (CallKind::Other, "called", "tool", "tools"),
        (CallKind::Listing, "listed", "directory", "directories"),
    ] {
        let these = of(want);
        if !these.is_empty() {
            clauses.push(
                Clause::new(format!("{verb} {}", counted(these.len(), one, many))).failed(failed(&these)),
            );
        }
    }
    let todos = of(CallKind::Todo);
    if !todos.is_empty() {
        clauses.push(Clause::new("updated todos").failed(failed(&todos)));
    }
    // slopty: what the commands landed ("committed 1a2b3c4", "pushed to main").
    let settled: Vec<ToolItem> = tools.iter().map(|t| (*t).clone()).collect();
    clauses.extend(landed_words(&settled).into_iter().map(Clause::new));
    clauses
}
// slopty: a call that only lists a directory, as the TUI's group line counts it.
fn lists_directory(call: &ToolCall) -> bool {
    match call {
        ToolCall::Exec { command } => {
            let first = command.split_whitespace().next().unwrap_or_default();
            let lone = !command.contains(['|', ';', '&', '>']);
            lone && matches!(first, "ls" | "tree" | "eza" | "exa" | "dir")
        }
        ToolCall::Unknown { name, .. } => name == "LS",
        _ => false,
    }
}

// slopty: what the group's commands landed, each once, in order.
fn landed_words(tools: &[ToolItem]) -> Vec<String> {
    let mut said: Vec<String> = Vec::new();
    for words in tools.iter().filter_map(|t| t.facts.as_ref().and_then(|facts| facts.landed.as_deref())) {
        for word in words.split(" · ") {
            if !said.iter().any(|s| s == word) {
                said.push(word.to_owned());
            }
        }
    }
    said
}

// slopty: a title under way shimmers as every live line in the app does (opencode's
// `text-shimmer`, `.research/opencode-study-2026-10-19.md` item 1): `motion::shimmer_ink` inks each
// glyph from faint to full as the band crosses it, on the process's one clock, so the group's
// summary and its running row move in step with the working line. Settled, the title is plain
// and takes its row's ink.
/// `text`, shimmering at `shimmer_ms` into the shimmer's cycle, or plain without one, its
/// `marks` drawn apart (`Sentence`).
fn tool_group_title(
    text: SharedString,
    marks: &[(Range<usize>, Mark)],
    shimmer_ms: Option<f32>,
    theme: &Theme,
) -> AnyElement {
    if let Some(phase) = shimmer_ms {
        let runs = title_shimmer_runs(&text, phase, theme);
        return StyledText::new(text).with_highlights(runs).into_any_element();
    }
    if marks.is_empty() {
        return text.into_any_element();
    }
    let runs = marks.iter().map(|(range, mark)| {
        let ink = match mark {
            Mark::Join => theme.text_faint,
            Mark::Added => theme.diff_add,
            Mark::Removed => theme.diff_del,
        };
        (range.clone(), gpui::HighlightStyle { color: Some(ink), ..Default::default() })
    });
    StyledText::new(text).with_highlights(runs.collect::<Vec<_>>()).into_any_element()
}

/// slopty: the ink of each glyph of `text` at `phase` ms into the shimmer's cycle.
fn title_shimmer_runs(text: &str, phase: f32, theme: &Theme) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
    text.char_indices()
        .enumerate()
        .map(|(index, (start, ch))| {
            let ink = motion::shimmer_color(motion::shimmer_ink(index, phase), theme);
            (start..start + ch.len_utf8(), gpui::HighlightStyle { color: Some(ink), ..Default::default() })
        })
        .collect()
}

/// slopty: where the shimmer stands at `moment`, in milliseconds into its cycle.
fn shimmer_ms(moment: Instant) -> f32 {
    let cycle = Duration::from_secs_f32(motion::SHIMMER_PERIOD_MS / 1000.0);
    motion::locked_phase(cycle, moment) * motion::SHIMMER_PERIOD_MS
}

// `single_line` and the per-kind chip label/detail are shared with the terminal
// viewport (`slopty_view_proto::view`): a tool must be named identically on every
// surface, and the one-line collapse is needed for the same reason in both (a
// literal newline breaks gpui's ellipsis logic and would be a cursor move in a
// cell grid).
pub use slopty_view_proto::view::{single_line, tool_chip_content};

/// Analytic expanded-chips height — no measurement needed for the fold tween.
pub fn chips_height(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    CHIPS_TOP_PAD + count as f32 * CHIP_HEIGHT + (count as f32 - 1.0) * CHIP_GAP
}

/// Analytic height an open detail adds to its chip's card (separator + body)
/// — output blocks by line count, diff blocks via the changes pane's own
/// [`crate::changes::body_height`]. The chip's own [`CHIP_HEIGHT`] is already
/// counted by [`chips_height`].
pub fn detail_height(detail: &ToolDetail) -> f32 {
    let body = match detail {
        ToolDetail::Output {
            lines,
            truncated_by,
            ..
        } => {
            let rows = lines.len() + usize::from(*truncated_by > 0);
            rows as f32 * OUTPUT_LINE_HEIGHT + OUTPUT_BODY_PAD
        }
        ToolDetail::Thought {
            lines,
            truncated_by,
        } => {
            let rows = lines.len() + usize::from(*truncated_by > 0);
            rows as f32 * OUTPUT_LINE_HEIGHT + OUTPUT_BODY_PAD
        }
        ToolDetail::Diff { file, .. } => crate::changes::body_height(file),
        ToolDetail::Stats { stats } => stats.len() as f32 * OUTPUT_LINE_HEIGHT + OUTPUT_BODY_PAD,
    };
    DETAIL_SEPARATOR + body
}

/// Tool image preview frame height; widths follow each image's aspect.
const TOOL_IMAGE_HEIGHT: f32 = 220.0;
const TOOL_IMAGE_MAX_WIDTH: f32 = 560.0;
const TOOL_IMAGE_PAD: f32 = 8.0;
/// Analytic height an open chip's image strip adds (separator + frames).
const TOOL_IMAGE_STRIP_HEIGHT: f32 = DETAIL_SEPARATOR + TOOL_IMAGE_HEIGHT + 2.0 * TOOL_IMAGE_PAD;

/// The open image context menu: what to copy and where it lives.
struct ImageMenu {
    path: String,
    devices: Vec<String>,
    position: Point<Pixels>,
    active: Option<usize>,
}

/// Spawn chips never expand, so they never preview images either.
fn chip_has_images(tool: &ToolItem) -> bool {
    !tool.images.is_empty() && !is_spawn_link(tool)
}

/// slopty: a read that went through shows its path and nothing more (Amp): what it read is the
/// file, which its path opens; only a failed read, or one that read an image, opens in place.
fn quiet_read(tool: &ToolItem) -> bool {
    matches!(tool.call, ToolCall::ReadFile { .. }) && !tool.is_error && !chip_has_images(tool)
}

/// Height of the "Show full output/diff" affordance row appended below an
/// open detail whose full payload lives in the sidecar (chat2-sync A3).
pub const BLOB_AFFORDANCE_HEIGHT: f32 = 24.0;

/// What an open chip's [`BLOB_AFFORDANCE_HEIGHT`] row offers: a lazy sidecar
/// fetch ("Show full output/diff"). One slot, so the analytic height sums
/// stay a single `is_some` check.
#[derive(Clone)]
struct ChipAffordance {
    blob_ref: SharedString,
    label: SharedString,
    /// slopty: the fetch failed; a "Retry" follows the words (#18).
    retry: bool,
}

/// Line cap for a FETCHED full output (a defensive ceiling, not a doc cap —
/// the harness bounds outputs at 4KiB, so this is rarely reached).
const FULL_OUTPUT_MAX_LINES: usize = 400;

/// Build the upgraded detail from a fetched sidecar blob. Diff blobs parse
/// the `ToolDiff` JSON through the same pipeline as inline diffs; output
/// blobs render (near-)uncapped — fetching past the summary was the point.
fn blob_detail(text: &str, is_diff: bool) -> Option<ToolDetail> {
    if is_diff {
        let diff: slopty_view_proto::ToolDiff = serde_json::from_str(text).ok()?;
        return tool_detail(None, Some(&diff), None);
    }
    // slopty: the whole output reads as its summary does, JSON pretty-printed.
    let mut lines = output_lines(text);
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let truncated_by = lines.len().saturating_sub(FULL_OUTPUT_MAX_LINES);
    lines.truncate(FULL_OUTPUT_MAX_LINES);
    Some(ToolDetail::Output {
        lines,
        truncated_by,
        stderr_from: None,
    })
}


// ---------------------------------------------------------------------------
// Working indicator flavour (pure; rendered by the shell strip)
// ---------------------------------------------------------------------------

/// Rotating flavour vocabulary (21 words / 7s, seeded per chat).
pub const FLAVOUR_WORDS: [&str; 21] = [
    "Sloptying",
    "Thinking",
    "Pondering",
    "Scheming",
    "Brewing",
    "Weaving",
    "Tinkering",
    "Musing",
    "Composing",
    "Sifting",
    "Untangling",
    "Distilling",
    "Sketching",
    "Plotting",
    "Riffing",
    "Combobulating",
    "Percolating",
    "Marinating",
    "Noodling",
    "Puzzling",
    "Conjuring",
];
pub const FLAVOUR_ROTATE_SECS: i64 = 7;

/// The flavour word for a seed at an elapsed time.
pub fn flavour_word(seed: u64, elapsed_secs: i64) -> &'static str {
    let step = (elapsed_secs.max(0) / FLAVOUR_ROTATE_SECS) as u64;
    FLAVOUR_WORDS[((seed.wrapping_add(step)) % FLAVOUR_WORDS.len() as u64) as usize]
}

/// A stable per-chat seed.
pub fn flavour_seed(chat_id: &str) -> u64 {
    fnv1a(chat_id.as_bytes())
}

/// Compact elapsed formatting, using at most two units up to days.
pub fn format_elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    // slopty: a second unit of nothing is left out ("1m", never "1m 0s"), as
    // `slopty_view_doc::work::duration_words` says it.
    slopty_view_doc::work::duration_words(u64::try_from(secs).unwrap_or_default().saturating_mul(1_000))
}

/// slopty: a timer that ticks: [`format_elapsed`] with its second unit padded ("1m 05s",
/// "1h 05m"), so in tabular figures the line holds its width as it counts.
pub fn ticking_elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h {:02}m", secs / 3_600, (secs % 3_600) / 60)
    } else {
        format!("{}d {:02}h", secs / 86_400, (secs % 86_400) / 3_600)
    }
}

// ---------------------------------------------------------------------------
// Highlight store (background, time-sliced, paint-only)
// ---------------------------------------------------------------------------

struct HighlightEntry {
    key: DocumentHighlightKey,
    document: Option<Weak<slopty_syntax::HighlightedDocument>>,
    _task: Option<Task<()>>,
}

/// Cache of tokenized code blocks keyed by `(row id, block ix)`. Tokenization
/// runs on the background executor, time-sliced; results apply as paint-only
/// run colors when they land.
#[derive(Default)]
struct HighlightStore {
    entries: HashMap<(SharedString, usize), HighlightEntry>,
    cache: SyntaxHighlightCache,
}

impl HighlightStore {
    /// Current tokens if ready; kicks a background tokenize when stale/missing.
    fn request(
        &mut self,
        row_id: SharedString,
        block_ix: usize,
        lang: Lang,
        code: &str,
        cx: &mut Context<Transcript>,
    ) -> Option<Arc<slopty_syntax::HighlightedDocument>> {
        let slot_key = (row_id.clone(), block_ix);
        let document_key = DocumentHighlightKey::new(lang, code);
        if let Some(entry) = self.entries.get(&slot_key)
            && entry.key == document_key
        {
            let document = entry.document.as_ref()?;
            if let Some(document) = document.upgrade() {
                return Some(document);
            }
        }
        if let Some(document) = self.cache.get(&document_key) {
            self.entries.insert(
                slot_key,
                HighlightEntry {
                    key: document_key,
                    document: Some(Arc::downgrade(&document)),
                    _task: None,
                },
            );
            return Some(document);
        }
        let code = code.to_string();
        let source_bytes = code.len();
        let task = cx.spawn(async move |this, cx| {
            let started = crate::clock::stopwatch();
            let document = cx
                .background_executor()
                .spawn(async move {
                    slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                        source: &code,
                        path: None,
                        fence_tag: Some(match lang {
                            Lang::Rust => "rust",
                            Lang::JavaScript => "javascript",
                            Lang::Jsx => "jsx",
                            Lang::TypeScript => "typescript",
                            Lang::Tsx => "tsx",
                            Lang::Python => "python",
                            Lang::Go => "go",
                            Lang::Json => "json",
                            Lang::Jsonc => "jsonc",
                            Lang::Bash => "bash",
                            Lang::Toml => "toml",
                            Lang::Markdown => "markdown",
                            Lang::Html => "html",
                            Lang::Css => "css",
                            Lang::Yaml => "yaml",
                            Lang::C => "c",
                            Lang::Cpp => "cpp",
                            Lang::CSharp => "csharp",
                            Lang::Java => "java",
                            Lang::Kotlin => "kotlin",
                            Lang::Swift => "swift",
                            Lang::Ruby => "ruby",
                            Lang::Php => "php",
                            Lang::Sql => "sql",
                            Lang::Lua => "lua",
                            Lang::Luau => "luau",
                            Lang::Dockerfile => "dockerfile",
                            Lang::Nix => "nix",
                            Lang::Make => "make",
                        }),
                    })
                    .ok()
                })
                .await;
            this.update(cx, |transcript, cx| {
                if let Some(document) = document {
                    let document = Arc::new(document);
                    let retained = transcript
                        .highlights
                        .cache
                        .insert(document_key, document.clone());
                    if let Some(entry) = transcript.highlights.entries.get_mut(&slot_key)
                        && entry.key == document_key
                    {
                        tracing::debug!(
                            language = ?lang,
                            source_bytes,
                            spans = document.lines.iter().map(Vec::len).sum::<usize>(),
                            elapsed_us = crate::clock::spent(started).as_micros() as u64,
                            "syntax highlight ready"
                        );
                        entry.document = retained.then(|| Arc::downgrade(&document));
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.entries.insert(
            (row_id, block_ix),
            HighlightEntry {
                key: document_key,
                document: None,
                _task: Some(task),
            },
        );
        None
    }
}

// ---------------------------------------------------------------------------
// Transcript entity
// ---------------------------------------------------------------------------

/// Expensive presentation work belongs to the subscription's background job,
/// never to a GPUI observer/render callback. Shared rows survive navigation.
#[derive(Default)]
pub(crate) struct TranscriptPreparation {
    entries: Vec<SessionMessageEntry>,
    cache: HashMap<String, Arc<Vec<Row>>>,
    live_parsers: HashMap<String, IncrementalParser>,
    tree_cache: HashMap<String, (usize, Arc<BlockTree>)>,
    baseline: Option<slopty_view_doc::TranscriptBaseline>,
}

pub(crate) struct PreparedTranscript {
    pub(crate) rows: HashMap<String, Arc<Vec<Row>>>,
    pub(crate) historical: HashMap<String, Vec<Row>>,
    fully_historical: HashSet<String>,
    pub(crate) navigation_baseline: Arc<slopty_view_doc::TranscriptBaseline>,
    pub(crate) bytes: usize,
}

impl TranscriptPreparation {
    pub(crate) fn prepare(
        &mut self,
        update: &slopty_view_doc::TranscriptUpdate,
    ) -> Result<Arc<PreparedTranscript>, slopty_view_doc::TranscriptDesync> {
        match &update.frame {
            slopty_view_doc::TranscriptFrame::Reset { .. } => {
                self.cache.clear();
                self.tree_cache.clear();
                self.live_parsers.clear();
            }
            slopty_view_doc::TranscriptFrame::Delta {
                upsert,
                append,
                remove,
                ..
            } => {
                if !upsert.is_empty() {
                    self.tree_cache.clear();
                }
                for id in upsert
                    .iter()
                    .map(|u| &u.entry.id)
                    .chain(append.iter().map(|a| &a.entry))
                    .chain(remove.iter())
                {
                    self.cache.remove(id);
                }
            }
        }
        slopty_view_doc::apply_transcript_frame(&mut self.entries, update.frame.clone())?;
        if let Some(baseline) = &update.replay_baseline {
            self.baseline = Some(baseline.clone());
        }
        let mut rows = HashMap::new();
        let mut historical = HashMap::new();
        let mut fully_historical = HashSet::new();
        let mut bytes = 0;
        for entry in &self.entries {
            let built = if let Some(rows) = self.cache.get(&entry.id) {
                rows.clone()
            } else {
                let streaming = entry.status == Some(MessageStatus::Streaming);
                let built = Arc::new(rows_for_entry(entry, false, &mut |key, text| {
                    parse_for_row(
                        streaming,
                        key,
                        text,
                        &mut self.live_parsers,
                        &mut self.tree_cache,
                    )
                    .0
                }));
                self.cache.insert(entry.id.clone(), built.clone());
                built
            };
            rows.insert(entry.id.clone(), built);
            if self.baseline.as_ref().is_some_and(|b| b.covers(entry)) {
                fully_historical.insert(entry.id.clone());
            }
            if let Some(baseline) = &self.baseline
                && !fully_historical.contains(&entry.id)
                && let Some(prefix) = baseline.historical_entry(entry)
            {
                historical.insert(
                    entry.id.clone(),
                    rows_for_entry(&prefix, false, &mut |_, text| {
                        Arc::new(parse_full(text))
                    }),
                );
            }
            bytes += std::mem::size_of::<SessionMessageEntry>()
                + entry.id.len()
                + entry
                    .parts
                    .iter()
                    .map(|part| std::mem::size_of::<MessagePart>() + part.byte_len())
                    .sum::<usize>();
        }
        self.cache.retain(|id, _| rows.contains_key(id));
        Ok(Arc::new(PreparedTranscript {
            rows,
            historical,
            fully_historical,
            bytes,
            navigation_baseline: Arc::new(slopty_view_doc::TranscriptBaseline::capture(&self.entries)),
        }))
    }
}

struct CachedRows {
    fingerprint: u64,
    rows: Vec<Row>,
}

#[derive(Default, Clone, Copy)]
struct FoldState {
    /// User pin (click); `None` follows the auto-open rule.
    open: Option<bool>,
    /// Bumped per toggle — keys the 200ms height tween.
    epoch: usize,
    /// Height at the moment of the toggle (the tween's start). The destination
    /// is always the *current* target height, so content growth after a toggle
    /// snaps instead of replaying a stale tween.
    from: f32,
    /// When the toggle happened. The tween is armed only for a short window
    /// after the click: gpui replays an element's animation on REMOUNT, and a
    /// virtualized row scrolling back into view is a remount — an armed-forever
    /// tween made every once-collapsed group flash open→closed on each
    /// reappearance (user report).
    toggled_at: Option<Instant>,
    disclosure_at: Option<Instant>,
    /// Per-toggle duration. User bubbles scale this with travel distance;
    /// existing tool folds leave it at zero and keep their catalog constants.
    duration_ms: u64,
    /// Extra user-body height revealed by Show more. It is not reply growth
    /// and must not permanently consume the sent turn's reservation.
    user_expansion_height: f32,
}

/// Reveal epochs for one live ordinary tool group. `None` means the row was
/// already present when this transcript attached (or has finished revealing),
/// so replaying history and scrolling a virtualized row back into view stay
/// completely still.
#[derive(Default)]
struct ToolGroupReveal {
    /// A newly streamed task header participates in the same height/fade/lift
    /// reveal as its steps. Replayed headers leave this unset.
    header_started_at: Option<Instant>,
    starts: Vec<Option<Instant>>,
    rendered_open: Option<bool>,
    rendered_height: f32,
}

fn tool_row_reveal_progress(start: Option<Instant>, now: Instant, reduce_motion: bool) -> f32 {
    let Some(start) = start.filter(|_| !reduce_motion) else {
        return 1.0;
    };
    let elapsed = now.checked_duration_since(start).unwrap_or_default();
    let raw = elapsed.as_secs_f32() / TOOL_ROW_REVEAL.total().as_secs_f32();
    TOOL_ROW_REVEAL.curve.eval(raw)
}

fn tool_connector_reveal_progress(
    start: Option<Instant>,
    now: Instant,
    reduce_motion: bool,
) -> f32 {
    let Some(start) = start.filter(|_| !reduce_motion) else {
        return 1.0;
    };
    let elapsed = now.checked_duration_since(start).unwrap_or_default();
    let raw = elapsed.as_secs_f32() / TOOL_CONNECTOR_REVEAL.total().as_secs_f32();
    TOOL_CONNECTOR_REVEAL.curve.eval(raw)
}

/// Split one arrival into a continuous tree draw. For child rows, the previous
/// row grows its continuation to the boundary first, then this row draws its
/// incoming trunk and elbow. The branch slightly overlaps the end of the
/// incoming phase so there is no dead frame at the bend.
fn tool_connector_parts(progress: f32, has_predecessor: bool) -> (f32, f32) {
    let progress = progress.clamp(0.0, 1.0);
    let (incoming_start, incoming_end, branch_start) = if has_predecessor {
        (0.45, 0.72, 0.68)
    } else {
        (0.0, 0.62, 0.58)
    };
    let incoming = ((progress - incoming_start) / (incoming_end - incoming_start)).clamp(0.0, 1.0);
    let branch = ((progress - branch_start) / (1.0 - branch_start)).clamp(0.0, 1.0);
    (incoming, branch)
}

/// The outgoing trunk belongs visually to the row that is already present,
/// but its timing belongs to the next row's arrival.
fn tool_connector_continuation(next_progress: Option<f32>) -> f32 {
    next_progress
        .map(|progress| (progress / 0.45).clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

/// Reveal by distance along the elbow and straight leg, so changing the branch
/// length does not introduce a speed jump at their junction.
fn activity_branch_points(progress: f32) -> Vec<Point<f32>> {
    let mut path: Vec<_> = (0..=24)
        .map(|step| {
            let t = step as f32 / 24.0;
            point(
                ACTIVITY_BEND_RADIUS * t * t,
                ACTIVITY_BEND_RADIUS * (2.0 * t - t * t),
            )
        })
        .collect();
    path.push(point(
        ACTIVITY_BRANCH_END_X - ACTIVITY_TRUNK_X,
        ACTIVITY_BEND_RADIUS,
    ));
    if progress >= 1.0 {
        return path;
    }
    let lengths: Vec<_> = path
        .windows(2)
        .map(|pair| (pair[1].x - pair[0].x).hypot(pair[1].y - pair[0].y))
        .collect();
    let mut remaining = lengths.iter().sum::<f32>() * progress.clamp(0.0, 1.0);
    let mut visible = vec![path[0]];
    for (pair, length) in path.windows(2).zip(lengths) {
        if remaining <= 0.0 {
            break;
        }
        let t = (remaining / length).min(1.0);
        visible.push(point(
            motion::lerp(pair[0].x, pair[1].x, t),
            motion::lerp(pair[0].y, pair[1].y, t),
        ));
        remaining -= length;
    }
    visible
}

/// slopty: where a tool fold toggled toward `open` at `at` stands at `now`, as the one fold
/// moves (`motion::fold_frame`; design-inspiration item 7): the height opens on `FOLD_OPEN` and
/// the contents fade in and rise after it; closing, they are gone before it lands on
/// `FOLD_CLOSE`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ToolFold {
    /// The height's share of the way from where it stood to where it goes, 0 to 1.
    share: f32,
    /// The contents' opacity, and how far below their place they stand.
    opacity: f32,
    rise: f32,
    /// Whether any of it still moves.
    moving: bool,
}

fn tool_fold(open: bool, at: Option<Instant>, now: Instant, still: bool) -> ToolFold {
    let Some(at) = at.filter(|_| !still) else {
        return ToolFold { share: 1.0, opacity: if open { 1.0 } else { 0.0 }, rise: 0.0, moving: false };
    };
    let frame = motion::fold_frame(open, now.saturating_duration_since(at), false);
    let share = if open { frame.height } else { 1.0 - frame.height };
    let moving = share < 1.0 || (open && frame.opacity < 1.0);
    ToolFold { share, opacity: frame.opacity, rise: frame.rise, moving }
}

fn tool_disclosure_progress(open: bool, fold: FoldState, now: Instant) -> f32 {
    let share = tool_fold(open, fold.disclosure_at, now, false).share;
    if open { share } else { 1.0 - share }
}

/// Viewport compensation paired with a long user-message collapse. While the
/// row loses height, this scrolls upward by the same eased distance so a
/// bottom-pinned viewport keeps the collapsing bubble in view.
struct UserCollapseScroll {
    started_at: Instant,
    duration_ms: u64,
    height_delta: f32,
    row_ix: usize,
    initial_top: f32,
    target_top: f32,
}

/// slopty: a step asked for on the window's next frame, one in flight at a time. A view keeps
/// its note that it asked, but the callback it asked for can be lost: GPUI's check of a retained
/// frame draws the window again from scratch and drops the callbacks that draw asked for, while
/// the view keeps the note. A note that waited past [`FRAME_ASK_PATIENCE`] is asked again rather
/// than waited on for good (a send that never glided, a pin that never followed); the late
/// answer to an older ask is ignored, so a driver still steps once a frame.
#[derive(Clone, Copy, Debug, Default)]
struct FrameAsk {
    /// The latest ask's token.
    token: u64,
    /// When the latest ask still waiting was made.
    since: Option<Instant>,
}

/// How long an ask waits for its frame before it is asked again: a few frames, even at an
/// inactive window's 30 a second.
const FRAME_ASK_PATIENCE: Duration = Duration::from_millis(100);

impl FrameAsk {
    /// A new ask's token at `now`, unless one made within the patience still waits.
    fn ask(&mut self, now: Instant) -> Option<u64> {
        if self.since.is_some_and(|since| now.saturating_duration_since(since) < FRAME_ASK_PATIENCE) {
            return None;
        }
        self.token = self.token.wrapping_add(1);
        self.since = Some(now);
        Some(self.token)
    }

    /// Whether the frame answering `token` answers the latest ask, which is then over.
    fn answered(&mut self, token: u64) -> bool {
        let latest = token == self.token && self.since.is_some();
        if latest {
            self.since = None;
        }
        latest
    }

    /// Whether an ask waits for its frame.
    #[cfg(test)]
    const fn waiting(&self) -> bool {
        self.since.is_some()
    }
}



/// A stable per-chat viewport anchor. Row identity is preferred over its old
/// index because async replay can insert or remove rows while a chat is away.
#[derive(Clone, Debug)]
struct ViewportAnchor {
    row_id: SharedString,
    entry_id: SharedString,
    fallback_ix: usize,
    offset_in_row: Pixels,
}

impl ViewportAnchor {
    fn capture(rows: &[Row], scroll_top: ListOffset) -> Option<Self> {
        let fallback_ix = scroll_top.item_ix.min(rows.len().checked_sub(1)?);
        let row = &rows[fallback_ix];
        Some(Self {
            row_id: row.id.clone(),
            entry_id: row.entry_id.clone(),
            fallback_ix,
            offset_in_row: scroll_top.offset_in_item,
        })
    }

    fn resolve_exact(&self, rows: &[Row]) -> Option<ListOffset> {
        let item_ix = rows.iter().position(|row| row.id == self.row_id)?;
        Some(ListOffset {
            item_ix,
            offset_in_item: self.offset_in_row,
        })
    }

    fn resolve(&self, rows: &[Row]) -> Option<ListOffset> {
        if let Some(offset) = self.resolve_exact(rows) {
            return Some(offset);
        }

        // A row can disappear when a streaming block is reshaped. Stay in the
        // same message entry, choosing the surviving row nearest the old
        // location; the intra-row offset is no longer meaningful in that case.
        let item_ix = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.entry_id == self.entry_id)
            .min_by_key(|(ix, _)| ix.abs_diff(self.fallback_ix))
            .map(|(ix, _)| ix)
            .unwrap_or_else(|| self.fallback_ix.min(rows.len().saturating_sub(1)));
        (!rows.is_empty()).then_some(ListOffset {
            item_ix,
            offset_in_item: px(0.0),
        })
    }
}

/// Session-local viewport state. Chats that were following their tail keep
/// following it; only user-owned viewports restore a concrete row anchor.
#[derive(Clone, Debug)]
enum SavedViewport {
    FollowTail,
    Anchored {
        anchor: ViewportAnchor,
        distance_from_bottom: f32,
    },
}

struct RestoredViewport {
    offset: ListOffset,
    distance_from_bottom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ViewportFinalizeToken {
    generation: u64,
    layout_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptReplayState {
    Pending,
    Empty,
    Populated,
}

impl TranscriptReplayState {
    fn authoritative_empty(self) -> bool {
        self == Self::Empty
    }

    fn allows_fallback(self) -> bool {
        self == Self::Populated
    }
}

impl ViewportFinalizeToken {
    fn still_current(self, generation: u64) -> bool {
        self.generation == generation
    }

    fn layout_settled(self, layout_revision: u64) -> bool {
        self.layout_revision == layout_revision
    }
}

impl SavedViewport {
    fn capture(
        rows: &[Row],
        scroll_top: ListOffset,
        pinned: bool,
        distance_from_bottom: f32,
    ) -> Option<Self> {
        if rows.is_empty() {
            return None;
        }
        if pinned {
            return Some(Self::FollowTail);
        }
        Some(Self::Anchored {
            anchor: ViewportAnchor::capture(rows, scroll_top)?,
            distance_from_bottom,
        })
    }

    /// Before the opening reset arrives, rows may contain only optimistic
    /// echoes. In that gap an exact row is safe, but entry/index fallbacks
    /// would mistake an unrelated echo for the authoritative transcript.
    fn resolve(&self, rows: &[Row], allow_fallback: bool) -> Option<RestoredViewport> {
        let Self::Anchored {
            anchor,
            distance_from_bottom,
        } = self
        else {
            return None;
        };
        let offset = if allow_fallback {
            anchor.resolve(rows)?
        } else {
            anchor.resolve_exact(rows)?
        };
        Some(RestoredViewport {
            offset,
            distance_from_bottom: *distance_from_bottom,
        })
    }
}

#[derive(Default)]
struct SavedViewportCache {
    by_chat: HashMap<String, SavedViewport>,
    recency: VecDeque<String>,
}

impl SavedViewportCache {
    fn insert(&mut self, chat_id: String, viewport: SavedViewport) {
        if self.by_chat.contains_key(&chat_id) {
            self.recency.retain(|candidate| candidate != &chat_id);
        }
        self.recency.push_back(chat_id.clone());
        self.by_chat.insert(chat_id, viewport);
        while self.by_chat.len() > MAX_SAVED_VIEWPORTS {
            let Some(evicted) = self.recency.pop_front() else {
                break;
            };
            self.by_chat.remove(&evicted);
        }
    }

    fn get_cloned_and_touch(&mut self, chat_id: &str) -> Option<SavedViewport> {
        let viewport = self.by_chat.get(chat_id).cloned()?;
        self.recency.retain(|candidate| candidate != chat_id);
        self.recency.push_back(chat_id.to_string());
        Some(viewport)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_chat.len()
    }
}

pub struct Transcript {
    state: Entity<AppState>,
    list: ListState,
    /// slopty: whether `list` aligns its rows to its top: a subagent's run, in a doc of its own
    /// or drilled into as a chat, reads like a fresh notes page (#5).
    list_top: bool,
    rows: Vec<Row>,
    last_source: Option<(Option<String>, TranscriptReplayState, u64)>,
    chat_id: Option<String>,
    /// The shell may retain this already-laid-out view briefly for its exit.
    /// Cleared as soon as the exit is invisible; never used for another chat.
    retain_on_deselect: bool,
    /// slopty: what the list ends with, under its last row (the inspector's mirror of the TUI's
    /// prompt box, [`Self::set_footer`]).
    footer: Option<gpui::AnyView>,
    /// slopty: the press under way began on the footer, which is no row: it leaves the follow
    /// rule alone (`inspector.md`, "Follow and anchor": a press in a row lets go).
    footer_press: bool,
    /// `Some(doc_id)` pins this instance to a SUBAGENT doc: rows come from
    /// `AppState::sub_transcript(doc_id)` instead of the selected chat, and
    /// the instance is READ-ONLY — no echoes, no own-turn hold, and no global
    /// attachment protection (that set is shared with the primary transcript
    /// and overwritten wholesale).
    doc_override: Option<String>,
    /// Whether an override instance watches a LIVE doc (`for_doc(follow)`):
    /// only then may the working trailer render — a frozen snapshot must
    /// never spin, whatever its entries claim.
    doc_live: bool,
    /// Memory-only viewport state for primary chats visited in this window.
    /// A transcript instance is shared across tabs, so the active ListState is
    /// reset on every attach and cannot retain these positions by itself.
    saved_viewports: SavedViewportCache,
    /// slopty: the work groups the person opened or shut in each chat visited, so a chat left
    /// and come back to (a subagent drilled into, then Escape) reads as they left it, and its
    /// saved viewport lands on rows that are still drawn. Kept for the chats whose viewport is.
    saved_folds: HashMap<String, HashMap<SharedString, bool>>,
    /// An anchored viewport waiting for the selected chat's async replay.
    pending_viewport: Option<SavedViewport>,
    /// Generation of the selected chat, guarding post-layout restoration
    /// callbacks across rapid A→B→A navigation.
    viewport_generation: u64,
    /// A restored item anchor needs one post-layout refresh of distance-based
    /// UI state; programmatic list scrolling never invokes `handle_scroll`.
    viewport_finalize_pending: bool,
    viewport_finalize_scheduled: FrameAsk,
    /// Bumped whenever sync or own-turn logic invalidates measured rows. The
    /// post-restore finalizer waits until one layout completes without another
    /// invalidation, avoiding a stale jump-button decision.
    viewport_layout_revision: u64,
    /// One-shot "open at the latest content" for UNPINNED (frozen) override
    /// instances: rows land ASYNC after the tab opens (watch replay / blob
    /// fetch), so the end-scroll fires on the first non-empty sync, then
    /// never again — landing at the end and FOLLOWING it are different
    /// states, and the user owns the viewport from there. Pinned instances
    /// don't need it (the pin branch already opens at the end).
    land_end_pending: bool,
    row_cache: HashMap<String, CachedRows>,
    live_parsers: HashMap<String, IncrementalParser>,
    tree_cache: HashMap<String, (usize, Arc<BlockTree>)>,
    folds: HashMap<SharedString, FoldState>,
    /// Entrance state follows stable groups through completion so fast calls
    /// finish revealing. Replay rows have no entrance timestamps.
    tool_group_reveals: HashMap<SharedString, ToolGroupReveal>,
    last_replay_baseline: Option<Arc<slopty_view_doc::TranscriptBaseline>>,
    /// Parsed historical prefixes, used to seed text before a coalesced live
    /// suffix is painted. The wire watermark contains lengths, not text.
    historical_markdown: HashMap<SharedString, Row>,
    /// Detail folds (output/diff) per chip, keyed `"{row_id}#d{ix}"` — full
    /// [`FoldState`]s so detail bodies tween open/closed exactly like the
    /// group fold. Render-local like `folds` — never part of the row
    /// fingerprint.
    tool_details: HashMap<SharedString, FoldState>,
    /// Expand/collapse state for user bubbles past [`USER_COLLAPSED_LINES`],
    /// keyed by row id. Render-local like `folds` — never part of the row
    /// fingerprint, so toggling one costs a repaint, not a rebuild.
    user_folds: HashMap<SharedString, FoldState>,
    /// Full laid-out text heights for long user bubbles. The text's paint
    /// canvas writes these cells without notifying or mutating the transcript;
    /// click handlers read them as exact endpoints for the RESIZE tween. This
    /// preserves smooth layout motion without a paint → notify feedback loop.
    user_heights: HashMap<SharedString, Rc<Cell<f32>>>,
    /// Pending long-press toggle. A single task is enough because only one
    /// pointer can own a hold gesture at a time; a token invalidates stale
    /// timers when the pointer is released or moves into a text selection.
    user_hold_task: Option<Task<()>>,
    user_hold_token: u64,
    user_collapse_scroll: Option<UserCollapseScroll>,
    /// Tracks the queued frame, even when its animation is canceled/replaced.
    /// Only that callback clears it, so rapid input cannot fork frame drivers.
    user_collapse_scroll_scheduled: FrameAsk,
    /// Streaming fade veils, one per live markdown row (dropped on completion).
    veils: HashMap<SharedString, Rc<RefCell<RowVeil>>>,
    /// Live rows present in the transcript's REPLAY after (re)attaching to a
    /// chat: their veils are created pre-seeded, so text that was already
    /// streamed before the switch never fades in — only appends after it do
    /// (mugen's `FadePainter.attach` baseline; user report: switching back to
    /// a streaming session dissolved the entire reply).
    veil_baseline: std::collections::HashSet<SharedString>,
    /// Armed at attach, disarmed on the first sync whose transcript is
    /// non-empty: the baseline must be captured from the doc REPLAY frame,
    /// not the attach-time sync — selection clears the transcript and the
    /// replay lands async, so capturing at attach seeded nothing and the
    /// still-streaming reply faded in whole on every session switch (user
    /// report, round 2).
    veil_attach_pending: bool,
    /// Cross-frame flatten/shape-input cache (see [`RenderCache`]): fade
    /// frames reuse settled blocks' text+runs; the incremental parser's stable
    /// boundary invalidates only the live tail per commit.
    render_cache: Rc<RefCell<RenderCache>>,
    /// slopty: what each row's chat chips last said (`markdown/chat_links.rs`): a row whose
    /// chips say something new flattens again.
    chip_words: HashMap<SharedString, String>,
    workspace_link: Option<render::LinkUi>,
    /// File-link roots per linking chat, valid for one
    /// `AppState::link_roots_revision`: every rendered row asks for them.
    file_link_roots: (
        u64,
        HashMap<SharedString, Rc<Vec<crate::workspace_links::FileLinkRoot>>>,
    ),
    /// Inline code spans that name an existing file, rewritten into the
    /// Markdown links they stand for per text part (see
    /// [`crate::markdown::inline_code_links`]). Reset with the same
    /// link-roots revision that resets `file_link_roots`.
    inline_code_links: crate::markdown::inline_code_links::InlineCodeLinkCache,
    rendered_rows: HashSet<SharedString>,
    /// slopty: the entries of the shown transcript the agent did not say (the person's, a
    /// compaction note, a fork's seam): none opens a message in the agent's name (#8).
    unsaid_entries: HashSet<SharedString>,
    /// Last UI typography generation reflected in `list` item measurements.
    /// Family and size changes can alter prose wrapping without changing row
    /// identity, so the virtual list must explicitly discard cached heights.
    typography_generation: u32,
    content_width: f32,
    /// Width held free at the column's right for a docked side card; the
    /// reading column centers in what remains.
    right_reserve: f32,
    highlights: HighlightStore,
    /// slopty: the last row there was when the person left the end; the jump pill counts the
    /// rows after it as new (design-inspiration item 11).
    jump_mark: Option<SharedString>,
    /// slopty: each message strip fading in or out: whether it shows, where its fade started
    /// and when ([`Transcript::strip_fade`]).
    strip_fades: HashMap<SharedString, (bool, f32, Instant)>,
    /// Distance from the bottom at the person's last scroll.
    last_scroll_distance: f32,
    /// slopty: whether the view follows its end (the list's `FollowMode::Tail`), the follow
    /// rule's one state ([`Transcript::follow`], [`Transcript::let_go`]).
    pinned: bool,
    /// slopty: a jump's glide under way, stepped once a frame (`rail.rs`, #13), and whether
    /// its next frame is asked for.
    glide: Option<crate::rail::Glide>,
    glide_scheduled: FrameAsk,
    /// slopty: the row a jump landed on, when, and which landing it was: it flashes (#13).
    landed: Option<(SharedString, Instant, u64)>,
    /// Last pointer sample while markdown selection owns a left-button drag.
    selection_drag_position: Option<Point<Pixels>>,
    /// One-shot timer rescheduled only while the pointer remains in an edge
    /// zone. Dropping it on mouse-up stops all selection scroll work.
    selection_scroll_task: Option<Task<()>>,
    // slopty: no composer stack over the transcript's bottom, so no clearance under it.
    /// slopty: a subagent's transcript open in its card (`transcript/subagents.rs`), which
    /// takes the height its rows measure: told to the card after each layout.
    fit: Option<Fit>,
    /// slopty: the time the working line reads: taken when the transcript's state moves and on
    /// the working timer's tick, never as a frame is built, so a frame built again (from
    /// scratch, or for a hover) says the second the frame shown says.
    clock: chrono::DateTime<chrono::Utc>,
    /// slopty: the working timer's next tick, on the turn's next whole second.
    clock_tick: Option<Task<()>>,
    /// slopty: the list's viewport as this frame began. A page row reads it while the list lays
    /// out, when asking the list itself would borrow it twice.
    page_viewport: gpui::Bounds<Pixels>,
    /// `(row id, entry id)` under the pointer — reveals the entry's timestamp
    /// strip (slopty chat-view.tsx `group-hover`; the rows report hover
    /// themselves). Keyed by ROW so a row→row move within one entry can't
    /// clear the reveal when the old row's leave event arrives after the new
    /// row's enter (enter/leave order across rows is not guaranteed).
    hovered_entry: Option<(SharedString, SharedString)>,
    /// slopty: each drawn strip's controls' focus (Copy, Rewind), by its entry, and what redraws
    /// the strip as one gains or loses it: a focused control shows its strip as a hover does
    /// (#9). Only strips drawn in the last frame, or holding focus, keep theirs (#13).
    strip_focus: HashMap<SharedString, StripFocus>,
    /// slopty: the entries whose strip this frame drew, for pruning [`Self::strip_focus`].
    strip_drawn: HashSet<SharedString>,
    /// slopty: the bubbles last drawn pending, and when each one sent: it fades up to full (#15).
    pending_bubbles: HashSet<SharedString>,
    sent_bubbles: HashMap<SharedString, Instant>,
    /// slopty: the last row's lane (its working line, then its timestamp strip) and the
    /// tallest that lane has drawn while it stayed last: the lane never shrinks under it, so
    /// a Stop or a turn's end swaps the working line for the strip without moving a row.
    last_lane: Option<(SharedString, Rc<Cell<f32>>)>,
    /// Code block showing "Copied" feedback: `(row id, block ix)`, cleared by
    /// the companion task after ~1.2s.
    copied_code: Option<(SharedString, usize)>,
    copied_clear: Option<Task<()>>,
    /// Per-visible-fence horizontal offsets and scrollbar hover/drag state.
    /// Keys use the transcript's stable row identity, so streaming → settled
    /// rerenders keep their local scroll position without leaking state for
    /// blocks no longer present in the selected chat.
    code_fences: HashMap<SharedString, render::CodeFenceRuntime>,
    /// Transcript attachment being viewed full-size (click a user thumbnail).
    attachment_preview: Option<crate::attachments::PreviewImage>,
    /// Focused while the lightbox is open so Escape reaches it.
    attachment_preview_focus: gpui::FocusHandle,
    attachment_preview_return_focus: Option<gpui::FocusHandle>,
    /// Mermaid fences drawn as diagrams in settled Markdown blocks. Each
    /// row's media handler requests its fences while laying out, so only
    /// painted diagrams are rendered (see [`MermaidCache`]).
    diagrams: Rc<RefCell<MermaidCache>>,
    /// The media handler over `diagrams`, built once and shared by every row.
    diagram_media: Option<render::MediaUi>,
    // slopty: the images a reply's markdown names (`transcript/md_images.rs`).
    md_images: Rc<RefCell<md_images::MdImages>>,
    /// The single serialized diagram render loop, while requests remain.
    diagram_worker: Option<Task<()>>,
    /// Prepared diagram shown in the lightbox: its natural size frames the
    /// enlarged raster, which is released when the lightbox closes.
    diagram_zoom: Option<crate::image_media::MediaImage>,
    /// Expanded tool chips' image previews; freed shortly after collapse.
    tool_images: crate::tool_images::ToolImages,
    /// Right-click menu over any transcript image (Copy path / Copy image).
    image_menu: crate::popover::Popup<ImageMenu>,
    image_menu_focus: gpui::FocusHandle,
    image_copy: Option<Task<()>>,
    /// In-flight ReadAttachmentChunk loads, keyed by device, path and validation
    /// policy; results land in the global attachment cache.
    attachment_loads: HashMap<crate::attachments::AttachmentKey, Task<()>>,
    /// Inline `show_visual` visuals, keyed by row id.
    visuals: HashMap<SharedString, Entity<crate::visuals::VisualFrame>>,
    // slopty: background work's rows, subagents' cards, and pages shown inline, keyed by row id.
    work: work::WorkState,
    subagents: subagents::SubagentsState,
    // slopty: the right-click menu.
    context_menu: context_menu::ContextMenuState,
    pages: HashMap<SharedString, Entity<crate::page::PageCardView>>,
    fences: crate::page::FenceCards, // slopty: chart fences' cards (M)
    /// Scheduled retry wake-ups for errored sources (the 2s→15s ladder).
    attachment_retries: HashMap<(String, String), Task<()>>,
    // slopty: Slopty's own rows' state, and find in thread.
    extras: extras::ExtrasState,
    find: find::FindState,
    /// Sidecar blob fetches keyed by doc ref (`chatId/partId[.diff]`,
    /// chat2-sync A3). `Ready` holds the UPGRADED detail, built once on
    /// arrival — render swaps it in per chip; rows never rebuild for it.
    /// Deliberately NOT cleared on chat switch: refs are chat-qualified and a
    /// fetched blob stays valid.
    blob_details: HashMap<SharedString, BlobFetch>,
    /// Monotonic fetch order per blob ref: when a tool has BOTH a diff and
    /// an output blob fetched, the chip shows the one requested most
    /// recently (click "Show full output" after a diff → see the output).
    blob_fetch_order: HashMap<SharedString, u64>,
    blob_fetch_counter: u64,
    _observe: Subscription,
    _text_changes: Subscription,
}

/// One sidecar blob fetch's lifecycle.
enum BlobFetch {
    Loading(#[allow(dead_code)] Task<()>),
    /// Failed with the affordance re-armed as a retry.
    Failed,
    Ready(Arc<ToolDetail>),
}

/// Shell-facing events (the transcript itself hosts no surfaces).
#[derive(Debug, Clone)]
pub enum TranscriptEvent {
    // slopty: a subagent's card drills in; it no longer opens a right-pane tab.
    /// A subagent card's Open: the session view shows the subagent's run, `chat_id`, with a
    /// breadcrumb back to its parent (`docs/decisions/ui.md`, "Subagents live in their parent's
    /// chat").
    OpenSubagent { chat_id: String },
}

impl gpui::EventEmitter<TranscriptEvent> for Transcript {}

impl Transcript {
    pub(crate) fn set_workspace_link_handler(&mut self, handler: render::LinkUi) {
        self.workspace_link = Some(handler);
    }

    pub(crate) fn link_ui(&mut self, cx: &mut Context<Self>) -> Option<render::LinkUi> {
        let source = self
            .workspace_link
            .as_ref()
            .and_then(|link| link.source_session.clone())
            .or_else(|| self.chat_id.clone())?;
        self.link_ui_for(&SharedString::from(source), cx)
    }

    /// The workspace-link handler bound to `source_chat_id`: the linking
    /// chat's own checkout resolves first, then its parent's and this
    /// device's project roots, and the roots come along for the trailing
    /// open glyph and the file menu.
    fn link_ui_for(
        &mut self,
        source_chat_id: &SharedString,
        cx: &mut Context<Self>,
    ) -> Option<render::LinkUi> {
        let roots = self.file_link_roots(source_chat_id, cx);
        let source_local = self.state.read(cx).chat_is_local(source_chat_id);
        self.workspace_link.clone().map(|mut link| {
            link.source_session = Some(source_chat_id.to_string());
            link.source_local = source_local;
            link.file_roots = Some(roots);
            link
        })
    }

    /// The ordered checkouts a file link from `chat_id` may open against:
    /// the chat's own, its parent's, then this device's project roots. The
    /// memo keeps row rendering from rebuilding them every frame; a state
    /// change that can move a root clears it.
    fn file_link_roots(
        &mut self,
        chat_id: &SharedString,
        cx: &gpui::App,
    ) -> Rc<Vec<crate::workspace_links::FileLinkRoot>> {
        let state = self.state.read(cx);
        let (revision, memo) = &mut self.file_link_roots;
        if *revision != state.link_roots_revision {
            *revision = state.link_roots_revision;
            memo.clear();
        }
        memo.entry(chat_id.clone())
            .or_insert_with(|| Rc::new(state.file_link_roots(chat_id)))
            .clone()
    }

    /// The row's text with every inline code span that names an existing
    /// file rewritten into the Markdown link it stands for (see
    /// [`crate::markdown::inline_code_links`]). The walk is memoized per part
    /// and link-roots revision; a revision change also drops the flatten
    /// cache, whose entries were shaped with the previous roots' styling.
    fn inline_code_tree(
        &mut self,
        tree: &Arc<BlockTree>,
        ui: Option<&render::LinkUi>,
        cx: &gpui::App,
    ) -> Arc<BlockTree> {
        let Some(ui) = ui else {
            return tree.clone();
        };
        let Some(roots) = ui.file_roots.as_deref() else {
            return tree.clone();
        };
        let revision = self.state.read(cx).link_roots_revision;
        if self.inline_code_links.set_revision(revision) {
            self.render_cache.borrow_mut().clear();
        }
        self.inline_code_links
            .linked_tree(tree, roots, ui.source_local)
    }

    /// slopty: `tree` with its links to chats shown as live chips, each saying the chat's
    /// title and how it stands now (`markdown/chat_links.rs`). The flatten cache is kept per
    /// row, so a row whose chips say something new since it last drew flattens again.
    fn chat_chips(
        &mut self,
        row: &SharedString,
        tree: Arc<BlockTree>,
        cx: &gpui::App,
    ) -> Arc<BlockTree> {
        let state = self.state.read(cx);
        let said = RefCell::new(String::new());
        let label = |url: &str| {
            let chat = crate::links::parse_slopty_conversation_link(url).ok()?.chat_id;
            let found = state.chats.iter().find(|known| known.id == chat)?;
            let status = state.session_for(&chat).map(|session| session.status);
            let words = crate::markdown::chat_links::chip_label(found.title.as_deref(), status);
            said.borrow_mut().push_str(&words);
            said.borrow_mut().push('\n');
            Some(words)
        };
        let Some(chipped) = crate::markdown::chat_links::labelled(&tree, &label) else {
            return tree;
        };
        let said = said.into_inner();
        if self.chip_words.get(row) != Some(&said) {
            self.chip_words.insert(row.clone(), said);
            self.render_cache.borrow_mut().invalidate_row(row);
        }
        Arc::new(chipped)
    }

    /// File badges on tool chips open through the same workspace-link route
    /// as Markdown file links, so only chats with a link handler get
    /// clickable badges.
    fn tool_file_opener(&mut self, cx: &mut Context<Self>) -> Option<ToolFileOpener> {
        self.link_ui(cx).map(|link| ToolFileOpener { link })
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self::install_find(cx); // slopty: find's keys reach the main transcript.
        Self::build(state, None, true, cx)
    }

    /// A read-only transcript over one SUBAGENT doc (right-pane tab). The
    /// caller starts the feed (`watch_subagent_doc` or the frozen snapshot);
    /// this instance only renders whatever lands under `doc_id`. `follow` =
    /// the doc is live: engage the end-follow pin from the start. Either
    /// way the tab OPENS at the latest content — a frozen transcript lands
    /// at the end once, unpinned, and free-scrolls from there.
    pub fn for_doc(
        state: Entity<AppState>,
        doc_id: String,
        follow: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(state, Some(doc_id), follow, cx)
    }

    /// The list rows sit in: aligned to its top (`top`) or to its bottom.
    fn new_list(top: bool, cx: &mut Context<Self>) -> ListState {
        let alignment = if top { ListAlignment::Top } else { ListAlignment::Bottom };
        let list = ListState::new(0, alignment, px(OVERDRAW_PX));
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event: &ListScrollEvent, _window, cx| {
            weak.update(cx, |this: &mut Transcript, cx| {
                this.handle_scroll(event, cx)
            })
            .ok();
        });
        list
    }

    fn build(
        state: Entity<AppState>,
        doc_override: Option<String>,
        follow: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        // Override instances align TOP: a subagent transcript reads like a
        // fresh notes page — entries anchored at the top, streaming growing
        // into the empty space below, never rising from the pane's bottom.
        // Top alignment gets that structurally (a short list rests at the
        // top with no reservation pad), and the follow rule runs on top of it.
        let list_top = doc_override.is_some();
        let list = Self::new_list(list_top, cx);
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
        cx.on_release(|this: &mut Self, cx| {
            // slopty: children open in place stop being watched with their card.
            let state = this.state.clone();
            this.subagents.close_all(&state, cx);
            this.close_diagram_zoom(cx);
            crate::image_media::release_media(this.diagrams.borrow_mut().drain(), cx);
            this.tool_images.clear(cx);
        })
        .detach();
        let text_changes = cx.subscribe(
            &state,
            |this: &mut Self, state, event: &crate::state::TranscriptTextChanged, cx| {
                let doc_id = this
                    .doc_override
                    .as_deref()
                    .or_else(|| state.read(cx).selected_chat.as_deref());
                if doc_id == Some(event.doc_id.as_str()) {
                    this.sync(cx);
                }
            },
        );
        // `follow` is the initial pin: the primary transcript always opens
        // pinned; an override instance pins only while its doc is LIVE (a
        // frozen transcript reads top-down, free-scrolling). Short content
        // is at-end by definition (distance 0), so the pin is invisible
        // until streaming overflows the pane — then it follows, releases on
        // wheel-up, and resticks/jumps exactly like the main transcript.
        let pinned = follow;
        if pinned {
            list.set_follow_mode(FollowMode::Tail);
        }
        let mut this = Self {
            state,
            list,
            list_top,
            rows: Vec::new(),
            last_source: None,
            // Pre-set so `sync` never sees an attach edge — an override
            // instance must not reset (or re-pin) on selection changes.
            chat_id: doc_override.clone(),
            retain_on_deselect: false,
            footer: None,
            footer_press: false,
            land_end_pending: doc_override.is_some() && !follow,
            doc_live: doc_override.is_some() && follow,
            doc_override,
            saved_viewports: SavedViewportCache::default(),
            saved_folds: HashMap::new(),
            pending_viewport: None,
            viewport_generation: 0,
            viewport_finalize_pending: false,
            viewport_finalize_scheduled: FrameAsk::default(),
            viewport_layout_revision: 0,
            row_cache: HashMap::new(),
            live_parsers: HashMap::new(),
            tree_cache: HashMap::new(),
            folds: HashMap::new(),
            tool_group_reveals: HashMap::new(),
            last_replay_baseline: None,
            historical_markdown: HashMap::new(),
            tool_details: HashMap::new(),
            user_folds: HashMap::new(),
            user_heights: HashMap::new(),
            user_hold_task: None,
            user_hold_token: 0,
            user_collapse_scroll: None,
            user_collapse_scroll_scheduled: FrameAsk::default(),
            veils: HashMap::new(),
            veil_baseline: std::collections::HashSet::new(),
            veil_attach_pending: true,
            render_cache: Rc::new(RefCell::new(RenderCache::default())),
            chip_words: HashMap::new(),
            workspace_link: None,
            file_link_roots: Default::default(),
            inline_code_links: Default::default(),
            rendered_rows: HashSet::new(),
            unsaid_entries: HashSet::new(),
            typography_generation: crate::typography::generation(cx),
            // slopty: the window's width arrives with the first render.
            content_width: crate::settings::transcript_width(0.0, cx),
            right_reserve: 0.0,
            highlights: HighlightStore::default(),
            jump_mark: None,
            strip_fades: HashMap::new(),
            last_scroll_distance: 0.0,
            pinned,
            glide: None,
            glide_scheduled: FrameAsk::default(),
            landed: None,
            selection_drag_position: None,
            selection_scroll_task: None,
            fit: None,
            clock: crate::clock::utc(cx),
            clock_tick: None,
            page_viewport: gpui::Bounds::default(),
            hovered_entry: None,
            strip_focus: HashMap::new(),
            strip_drawn: HashSet::new(),
            pending_bubbles: HashSet::new(),
            sent_bubbles: HashMap::new(),
            last_lane: None,
            copied_code: None,
            copied_clear: None,
            code_fences: HashMap::new(),
            attachment_preview: None,
            attachment_preview_focus: cx.focus_handle(),
            attachment_preview_return_focus: None,
            diagrams: Rc::default(),
            diagram_media: None,
            md_images: Rc::default(),
            diagram_worker: None,
            diagram_zoom: None,
            tool_images: Default::default(),
            image_menu: Default::default(),
            image_menu_focus: cx.focus_handle(),
            image_copy: None,
            attachment_loads: HashMap::new(),
            visuals: HashMap::new(),
            work: Default::default(),      // slopty: background work,
            context_menu: Default::default(),
            subagents: Default::default(), // subagents' cards,
            pages: HashMap::new(),         // and pages.
            fences: Default::default(),    // slopty: and chart fences.
            attachment_retries: HashMap::new(),
            extras: Default::default(), // slopty: Slopty's own rows.
            find: Default::default(),
            blob_details: HashMap::new(),
            blob_fetch_order: HashMap::new(),
            blob_fetch_counter: 0,
            _observe: observe,
            _text_changes: text_changes,
        };
        this.sync(cx);
        this
    }

    /// Shell-driven: hold `reserve` px free on the right of the reading
    /// column (a docked side card), so the column and card center together.
    pub fn set_right_reserve(&mut self, reserve: f32, cx: &mut Context<Self>) {
        let reserve = reserve.max(0.0);
        if (self.right_reserve - reserve).abs() > 0.25 {
            self.right_reserve = reserve;
            // Rows rewrap at the new width, including virtual ones offscreen.
            self.list.remeasure();
            cx.notify();
        }
    }

    /// slopty: measure the rows' height after each layout into `height`, and have `card`
    /// drawn again when it moves (a subagent's card, which takes that height up to its room).
    pub(crate) fn fit_to(
        &mut self,
        card: gpui::WeakEntity<Transcript>,
        row: SharedString,
        height: Rc<Cell<Option<f32>>>,
    ) {
        self.fit = Some(Fit { card, row, height });
    }

    /// slopty: row `row` takes another height (a subagent's card opened, closed, or fit to its
    /// run): it is measured again, and the list's layer painted afresh with it, so no row under
    /// it is shown where it stood before.
    /// As a diagram's row does, it lets the follow rule take the new height: a list following
    /// its end stays there as the card grows, and one held by its first row holds it.
    pub(crate) fn row_resized(&mut self, row: &str, cx: &mut Context<Self>) {
        self.diagram_layout_changed(&[SharedString::from(row.to_owned())], cx);
        cx.notify();
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// slopty: end the list with `footer`, under its last row, as the TUI keeps its prompt box
    /// under the conversation: what grows there grows at the list's end, which the follow rule
    /// follows, so nothing above it moves (`docs/decisions/inspector.md`, "Fixed frame"). With
    /// no rows it stands at the foot alone.
    pub fn set_footer(&mut self, footer: Option<gpui::AnyView>, cx: &mut Context<Self>) {
        self.footer = footer;
        if let Some(last) = self.rows.len().checked_sub(1) {
            self.list.remeasure_items(last..last + 1);
        }
        cx.notify();
    }

    /// slopty: `footer` in a frame that marks a press begun on it, before the transcript's own
    /// press handler hears it ([`Self::footer_press`]).
    fn footer_frame(footer: gpui::AnyView, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w_full()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, _, _| this.footer_press = true))
            .child(footer)
    }

    pub(crate) fn list_state(&self) -> &ListState {
        &self.list
    }

    /// Snapshot the outgoing primary chat before its rows and ListState are
    /// reset. Empty rows never overwrite an older snapshot: during a rapid
    /// A→B→A switch, B's replay may not have arrived before leaving it again.
    fn remember_current_viewport(&mut self) {
        // Rows can already contain optimistic echoes while an older snapshot
        // is still waiting for the authoritative replay. Leaving again in
        // that window must preserve the older snapshot, not replace it with
        // the partial echo-only viewport.
        if self.pending_viewport.is_some() {
            return;
        }
        let Some(chat_id) = self.chat_id.clone() else {
            return;
        };
        let distance_from_bottom = if self.pinned {
            0.0
        } else {
            self.distance_from_bottom()
        };
        let Some(viewport) = SavedViewport::capture(
            &self.rows,
            self.list.logical_scroll_top(),
            self.pinned,
            distance_from_bottom,
        ) else {
            return;
        };
        self.saved_viewports.insert(chat_id, viewport);
    }

    /// slopty: keep the outgoing chat's opened and shut work groups for its return, and drop
    /// those of chats whose viewport is no longer kept, `incoming` aside.
    fn remember_folds(&mut self, incoming: Option<&str>) {
        if let Some(chat_id) = self.chat_id.clone() {
            let pins: HashMap<SharedString, bool> = self
                .folds
                .iter()
                .filter_map(|(id, fold)| fold.open.map(|open| (id.clone(), open)))
                .collect();
            if pins.is_empty() {
                self.saved_folds.remove(&chat_id);
            } else {
                self.saved_folds.insert(chat_id, pins);
            }
        }
        let kept = &self.saved_viewports;
        let outgoing = self.chat_id.as_deref();
        self.saved_folds.retain(|chat, _| {
            kept.by_chat.contains_key(chat) || Some(chat.as_str()) == incoming || Some(chat.as_str()) == outgoing
        });
    }

    /// Restore an exact optimistic row while replay is pending, enable stable
    /// fallbacks only after a populated reset, and retire snapshots proven
    /// absent by an empty reset. `scroll_to` remains valid while the virtual
    /// list measures restored rows on the following layout pass.
    fn restore_pending_viewport(&mut self, replay: TranscriptReplayState) -> bool {
        if self.pending_viewport.is_none() {
            return false;
        }
        if !self.rows.is_empty()
            && let Some(restored) = self
                .pending_viewport
                .as_ref()
                .and_then(|saved| saved.resolve(&self.rows, replay.allows_fallback()))
        {
            self.pending_viewport = None;
            self.list.scroll_to(restored.offset);
            self.last_scroll_distance = restored.distance_from_bottom;
            self.viewport_finalize_pending = true;
            return true;
        }

        if !replay.authoritative_empty() {
            return false;
        }
        // The reset's document rows, not the combined rows, define
        // authoritative emptiness. A matching optimistic row above remains
        // valid, but an unrelated echo must never become an index fallback
        // for old history.
        self.discard_pending_viewport();
        self.follow();
        true
    }

    /// Explicit user/navigation intent supersedes a replay-delayed restore.
    /// Replace its cache entry with tail-follow until current rows can be
    /// snapshotted normally on the next chat switch.
    pub(crate) fn discard_pending_viewport(&mut self) {
        if self.pending_viewport.take().is_some()
            && let Some(chat_id) = self.chat_id.clone()
        {
            self.saved_viewports
                .insert(chat_id, SavedViewport::FollowTail);
        }
    }

    pub(crate) fn state_entity(&self) -> &Entity<AppState> {
        &self.state
    }

    /// Hand viewport ownership to explicit rail/navigation input before its
    /// reduced-motion or animated branch moves the list.
    pub(crate) fn begin_scroll_navigation(&mut self) {
        self.discard_pending_viewport();
        self.cancel_user_hold();
        self.user_collapse_scroll = None;
        self.stop_automatic_scrolling();
    }

    fn stop_automatic_scrolling(&mut self) {
        self.let_go();
        self.glide = None;
    }

    /// slopty: follow the list's end: GPUI's tail mode snaps to it on every layout, so what
    /// grows at the end stays in view (`inspector.md`, "Follow and anchor").
    pub(crate) fn follow(&mut self) {
        self.pinned = true;
        self.jump_mark = None;
        self.last_scroll_distance = 0.0;
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// slopty: stop following, on the person's own move: the first row in view holds its place
    /// from here, whatever lands below, and the rows that do are counted for the jump pill.
    pub(crate) fn let_go(&mut self) {
        if !self.pinned {
            return;
        }
        self.materialize_scroll_anchor();
        self.list.set_follow_mode(FollowMode::Normal);
        self.pinned = false;
        self.jump_mark = self.rows.last().map(|row| row.id.clone());
    }

    /// slopty: store the glide after [`Self::begin_scroll_navigation`] (`rail.rs`).
    pub(crate) fn set_glide(&mut self, glide: Option<crate::rail::Glide>) {
        self.glide = glide;
    }

    /// slopty: the glide under way, taken to step it.
    pub(crate) fn take_glide(&mut self) -> Option<crate::rail::Glide> {
        self.glide.take()
    }

    /// slopty: the size and line height its prose reads at: a run in its card at the card's
    /// compact body, under the card's title, not larger than it (#24).
    fn markdown_body(&self) -> (f32, f32) {
        if self.fit.is_some() {
            (render::MD_COMPACT_TEXT_SIZE, render::MD_COMPACT_LINE_HEIGHT)
        } else {
            (render::MD_TEXT_SIZE, render::MD_LINE_HEIGHT)
        }
    }


    /// slopty: the bubble `row` drawn `pending` or not, and whether it is fading up from
    /// pending to sent (#15).
    fn bubble_sent(&mut self, row: &SharedString, pending: bool, cx: &Context<Self>) -> bool {
        let now = crate::clock::now(cx);
        if pending {
            self.pending_bubbles.insert(row.clone());
            self.sent_bubbles.remove(row);
            return false;
        }
        if self.pending_bubbles.remove(row) && !motion::reduced_motion(cx) {
            self.sent_bubbles.insert(row.clone(), now);
        }
        let Some(at) = self.sent_bubbles.get(row) else { return false };
        if now.saturating_duration_since(*at) < motion::FADE_QUICK.total() {
            return true;
        }
        self.sent_bubbles.remove(row);
        false
    }

    /// slopty: mark the row at `ix` a jump landed on: it flashes and fades (#13).
    pub(crate) fn flash_landing(&mut self, ix: usize, cx: &Context<Self>) {
        if motion::reduced_motion(cx) {
            return;
        }
        let epoch = self.landed.as_ref().map_or(0, |(_, _, epoch)| epoch + 1);
        self.landed = self.rows.get(ix).map(|row| (row.id.clone(), crate::clock::now(cx), epoch));
    }



    pub(crate) fn distance_from_bottom(&self) -> f32 {
        let max = f32::from(self.list.max_offset_for_scrollbar().y);
        let cur = f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        (max + cur).max(0.0)
    }



    /// slopty: the person moved the list (its wheel or touch; programmatic scrolls never come
    /// here): back within [`FOLLOW_PX`] of the end it follows again, and away from it, it lets go.
    fn handle_scroll(&mut self, _event: &ListScrollEvent, cx: &mut Context<Self>) {
        // Cancel synchronously, before a queued animation frame can undo the
        // wheel/touch input. Neither operation reads the borrowed ListState.
        self.user_collapse_scroll = None;
        self.cancel_user_hold();
        // slopty: a live page the scroll moved looks again at where it lies.
        let fences: Vec<_> = self.fences.borrow().values().cloned().collect();
        // Only those are touched: one updated keeps the scroll from being composited.
        for card in self.pages.values().chain(&fences) {
            if card.read(cx).placed_live() {
                card.update(cx, |_, cx| cx.notify());
            }
        }
        // The list calls this while holding its own borrow: reading it back now panics, so the
        // decision waits for the end of the effect cycle.
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            this.update(cx, |this: &mut Transcript, cx| {
                this.discard_pending_viewport();
                let distance = this.distance_from_bottom();
                this.last_scroll_distance = distance;
                if distance <= FOLLOW_PX {
                    this.follow();
                } else {
                    this.let_go();
                }
                cx.notify();
            })
            .ok();
        });
    }


    fn on_selection_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() || !crate::markdown::selection::is_dragging() {
            self.stop_selection_scroll();
            return;
        }
        self.selection_drag_position = Some(event.position);
        if render::update_drag_at(event.position) {
            cx.notify();
        }
        self.schedule_selection_scroll(cx);
    }

    fn on_selection_mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // slopty: a press in a row lets go of the end, so what it pressed stays where it is as
        // more arrives (`inspector.md`, "Follow and anchor"); a selection's drag too, before a
        // stream commit can virtualize its anchor. Keep the user bubble's long-press timer. A
        // press on the footer (the prompt box's mirror, no row) leaves the view following.
        let on_footer = std::mem::take(&mut self.footer_press);
        if !crate::markdown::selection::is_dragging() {
            if self.pinned && !on_footer {
                self.let_go();
                cx.notify();
            }
            return;
        }
        self.selection_drag_position = Some(event.position);
        self.discard_pending_viewport();
        self.user_collapse_scroll = None;
        self.stop_automatic_scrolling();
        self.materialize_scroll_anchor();
        cx.notify();
    }

    fn on_selection_mouse_up(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_selecting = self.selection_drag_position.is_some();
        self.stop_selection_scroll();
        if let Some(_text) = crate::markdown::selection::end_active_drag() {
            // X11 middle-click paste parity, including the case where the
            // anchor row has virtualized away and cannot receive mouse-up.
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            cx.write_to_primary(ClipboardItem::new_string(_text));
        }
        if was_selecting {
            self.last_scroll_distance = self.distance_from_bottom();
            cx.notify();
        }
    }

    fn stop_selection_scroll(&mut self) {
        self.selection_drag_position = None;
        self.selection_scroll_task = None;
    }

    fn schedule_selection_scroll(&mut self, cx: &mut Context<Self>) {
        if self.selection_scroll_task.is_some() || !crate::markdown::selection::is_dragging() {
            return;
        }
        let Some(position) = self.selection_drag_position else {
            return;
        };
        if selection_scroll_step(self.list.viewport_bounds(), position) == 0.0 {
            return;
        }
        self.selection_scroll_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SELECTION_SCROLL_TICK_MS))
                .await;
            let _ = this.update(cx, |transcript, cx| {
                transcript.selection_scroll_task = None;
                transcript.step_selection_scroll(cx);
            });
        }));
    }

    fn step_selection_scroll(&mut self, cx: &mut Context<Self>) {
        if !crate::markdown::selection::is_dragging() {
            self.stop_selection_scroll();
            return;
        }
        let Some(position) = self.selection_drag_position else {
            return;
        };
        let step = selection_scroll_step(self.list.viewport_bounds(), position);
        if step == 0.0 {
            return;
        }

        // Resolve against the registry painted after the previous step before
        // moving it again. This is what lets a stationary edge pointer consume
        // successive virtualized rows.
        render::update_drag_at(position);
        self.begin_scroll_navigation();
        self.list.scroll_by(px(step));
        self.last_scroll_distance = self.distance_from_bottom();
        cx.notify();
        self.schedule_selection_scroll(cx);
    }


    /// Convert a glued scroll offset (`None`/past-the-end — layout re-snaps
    /// it to the end each frame) into a concrete `{item, offset}` anchored at
    /// the first visible row, which layout holds still.
    fn materialize_scroll_anchor(&mut self) {
        if !self.is_glued() {
            return;
        }
        let vp_top = f32::from(self.list.viewport_bounds().top());
        for ix in 0..self.rows.len() {
            if let Some(bounds) = self.list.bounds_for_item(ix)
                && f32::from(bounds.bottom()) > vp_top + 0.5
            {
                self.list.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: px(vp_top - f32::from(bounds.top())),
                });
                return;
            }
        }
        // Bottom-aligned short lists expose no item bounds. Materialize
        // their actual end position using the measured height tree instead.
        // Preserve a negative first-row offset for the blank space above a
        // short chat; clamping it to zero would jump as the minimum is added.
        if !self.rows.is_empty() {
            let viewport_height = f32::from(self.list.viewport_bounds().size.height);
            self.list.scroll_by(px(-1.0));
            let content_height = -f32::from(self.list.scroll_px_offset_for_scrollbar().y) + 1.0;
            if content_height < viewport_height {
                self.list.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(content_height - viewport_height),
                });
            } else {
                self.list.scroll_by(px(1.0 - viewport_height));
            }
        }
    }







    /// Whether the transcript is currently pinned to the bottom.
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// slopty: whether the jump pill shows: the view does not follow, and rows arrived below
    /// since it let go, or it is more than [`SCROLL_BUTTON_THRESHOLD_PX`] from the end.
    pub fn jump_button_shown(&self) -> bool {
        !self.pinned && (self.jump_unseen() > 0 || self.last_scroll_distance > SCROLL_BUTTON_THRESHOLD_PX)
    }

    fn strip_fade(&mut self, key: &SharedString, shown: bool, now: Instant, still: bool) -> (f32, bool) {
        strip_fade(&mut self.strip_fades, key, shown, now, still || TOUCH)
    }

    /// slopty: how many rows arrived since the view let go of the end, for the jump pill to say.
    pub fn jump_unseen(&self) -> usize {
        let Some(mark) = self.jump_mark.as_ref().filter(|_| !self.pinned) else {
            return 0;
        };
        self.rows.iter().rposition(|row| &row.id == mark).map_or(0, |ix| self.rows.len() - ix - 1)
    }

    /// slopty: the jump pill, at the list's foot while it does not follow its end and something
    /// waits below; a run in its card or a subagent's doc has none.
    fn render_jump(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        const KEY: &str = "transcript-jump";
        if self.fit.is_some() || self.doc_override.is_some() {
            return None;
        }
        let now = crate::clock::now(cx);
        let leaving = jump::presence(KEY, self.jump_button_shown(), now)?;
        if leaving.is_some() {
            // Drawn again until it has gone, so a spent pill never lingers unseen.
            cx.notify();
        }
        let button = jump::button(
            KEY,
            "transcript-jump-hover",
            self.jump_unseen(),
            (motion::reduced_motion(cx), now),
            Theme::of(cx),
            cx.listener(|this, _, _, cx| this.jump_to_bottom(cx)),
        );
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(Theme::SPACE_MD))
                .flex()
                .justify_center()
                .child(jump::leave(KEY, leaving, button))
                .into_any_element(),
        )
    }

    /// slopty: the jump pill's press: back to the end, following it.
    pub fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.user_collapse_scroll = None;
        self.cancel_user_hold();
        self.discard_pending_viewport();
        self.glide = None;
        self.follow();
        cx.notify();
    }

    /// Whether the scroll offset is in a bottom-glued representation (`None`
    /// or anchored past the end) — states where the next layout hard-snaps to
    /// the new end instead of holding a pixel position.
    pub(crate) fn is_glued(&self) -> bool {
        self.list.logical_scroll_top().item_ix >= self.rows.len()
    }


    pub(crate) fn retain_for_route_exit(&mut self) {
        self.retain_on_deselect = true;
    }

    fn route_exit_pending(&self, cx: &gpui::App) -> bool {
        self.retain_on_deselect
            && self.doc_override.is_none()
            && self.state.read(cx).selected_chat.is_none()
            && self.chat_id.is_some()
    }

    pub(crate) fn finish_route_exit(&mut self, cx: &mut Context<Self>) {
        if self.state.read(cx).selected_chat.is_none() && self.chat_id.is_some() {
            self.retain_on_deselect = false;
            self.sync(cx);
            self.retain_on_deselect = true;
        }
    }

    /// Rebuild rows from app state; splice minimal ranges into the list.
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.clock = crate::clock::utc(cx);
        if self.retain_on_deselect
            && self.doc_override.is_none()
            && self.state.read(cx).selected_chat.is_none()
            && self.chat_id.is_some()
        {
            // A quick return can reuse this entity before the exit finishes.
            // Its next snapshot is still a replay, not newly arriving tools.
            self.veil_attach_pending = true;
            return;
        }
        let (selected, replay) = {
            let s = self.state.read(cx);
            match &self.doc_override {
                // Pinned to a subagent doc: `selected` equals `chat_id` by
                // construction, so the attach/reset branch below never fires,
                // and echoes stay empty (nothing is ever sent from here).
                Some(doc_id) => (Some(doc_id.clone()), TranscriptReplayState::Populated),
                None => {
                    let replay = if !s.transcript_replayed {
                        TranscriptReplayState::Pending
                    } else if s.transcript.is_empty() {
                        TranscriptReplayState::Empty
                    } else {
                        TranscriptReplayState::Populated
                    };
                    (s.selected_chat.clone(), replay)
                }
            }
        };

        // slopty: what the selected chat is, read each time the state moves: a chat drilled
        // into before the list lists it (a note tapped at launch) takes a subagent run's
        // alignment once it is listed, not only as it opens (#17).
        let run = {
            let state = self.state.read(cx);
            selected
                .as_deref()
                .and_then(|id| state.chats.iter().find(|chat| chat.id == id))
                .is_some_and(|chat| chat.subagent)
        };
        let top = self.doc_override.is_some() || run;
        if top != self.list_top {
            // slopty: a drilled subagent's run reads from the top, as one in its own doc does;
            // the list takes the alignment its chat reads in, its rows laid in again (#5).
            self.list = Self::new_list(top, cx);
            self.list_top = top;
            self.rows.clear();
            self.last_source = None;
        }

        let source = (
            selected.clone(),
            replay,
            self.state.read(cx).transcript_revision,
        );
        if self.last_source.as_ref() == Some(&source) {
            return;
        }
        self.last_source = Some(source);

        let attached = selected != self.chat_id;
        // Arm the replay baseline before classifying tool arrivals. Selection
        // and replay may arrive in one sync; a retained same-chat entity can
        // also see a fresh pending subscription without changing chat_id.
        if attached || replay == TranscriptReplayState::Pending {
            self.veil_baseline.clear();
            self.veil_attach_pending = true;
        }
        if attached {
            // slopty: another chat's cards; the children this one opened close.
            let state = self.state.clone();
            self.subagents.close_all(&state, cx);
            self.pages.clear();
            self.fences.borrow_mut().clear(); // slopty: chart fences too (M)
            // Read the incoming snapshot before inserting the outgoing one:
            // a full bounded cache may evict its oldest entry, which can be
            // exactly the chat the user is reopening.
            let saved_viewport = selected
                .as_ref()
                .and_then(|chat_id| self.saved_viewports.get_cloned_and_touch(chat_id));
            self.remember_current_viewport();
            self.remember_folds(selected.as_deref());
            self.chat_id = selected;
            self.rows.clear();
            self.tool_images.clear(cx);
            self.md_images.borrow_mut().clear(); // slopty
            self.row_cache.clear();
            self.live_parsers.clear();
            self.tree_cache.clear();
            self.folds.clear();
            // slopty: the incoming chat's work opens as the person left it, before its rows are
            // built, so its saved viewport resolves against them.
            let pins = self.chat_id.as_ref().and_then(|chat| self.saved_folds.get(chat));
            for (id, open) in pins.into_iter().flatten() {
                self.folds.insert(id.clone(), FoldState { open: Some(*open), ..FoldState::default() });
            }
            self.tool_group_reveals.clear();
            self.last_replay_baseline = None;
            self.historical_markdown.clear();
            self.user_folds.clear();
            self.user_heights.clear();
            self.strip_focus.clear(); // slopty
            // slopty: the row the pointer was on left with its chat, unmounted with no leave, so
            // its strip must not come back lit on the return.
            self.hovered_entry = None;
            self.pending_bubbles.clear(); // slopty
            self.sent_bubbles.clear(); // slopty
            self.user_hold_token = self.user_hold_token.wrapping_add(1);
            self.user_hold_task = None;
            self.user_collapse_scroll = None;
            self.veils.clear();
            self.render_cache.borrow_mut().clear();
            self.highlights.entries.clear();
            self.list.reset(0);
            self.pending_viewport = None;
            self.viewport_generation = self.viewport_generation.wrapping_add(1);
            self.viewport_finalize_pending = false;
            self.jump_mark = None;
            if let Some(SavedViewport::Anchored {
                anchor,
                distance_from_bottom,
            }) = saved_viewport
            {
                // slopty: back where the person was reading, not following; the rows it resolves
                // against arrive with the replay.
                self.pinned = false;
                self.list.set_follow_mode(FollowMode::Normal);
                self.last_scroll_distance = distance_from_bottom;
                self.pending_viewport = Some(SavedViewport::Anchored {
                    anchor,
                    distance_from_bottom,
                });
            } else {
                // New chats and chats that were following their tail open at their end.
                self.follow();
            }
            self.glide = None;
            self.stop_selection_scroll();
        }

        let mut new_rows: Vec<Row> = Vec::new();
        // Borrow the transcript only while deriving rows. Cloning the entity
        // handle lets rows_for mutate our caches without copying every text
        // and tool payload on each app-state notification.
        let entries_empty = {
            let state = self.state.clone();
            let state = state.read(cx);
            let entries = match &self.doc_override {
                Some(doc_id) => state.sub_transcript(doc_id),
                None => state.transcript.as_slice(),
            };
            let prepared = self
                .chat_id
                .as_ref()
                .and_then(|id| state.prepared_transcripts.get(id));
            // slopty: the entries the agent did not say (the person's, a compaction, a fork's
            // seam, a relayed marker).
            self.unsaid_entries = entries
                .iter()
                .filter(|entry| entry.role != MessageRole::Assistant)
                .map(|entry| SharedString::from(entry.id.clone()))
                .collect();
            for entry in entries {
                if let Some(rows) = prepared.and_then(|p| p.rows.get(&entry.id)) {
                    // slopty: a plan's fold hides its rows here too.
                    new_rows.extend(rows.iter().filter(|row| self.fold_shows(row)).cloned());
                } else {
                    new_rows.extend(self.rows_for(entry, false));
                }
            }
            entries.is_empty()
        };

        let baseline = self
            .chat_id
            .as_deref()
            .and_then(|id| self.state.read(cx).transcript_baseline(id))
            .cloned();
        let baseline_changed = baseline.as_ref().is_some_and(|baseline| {
            self.last_replay_baseline
                .as_ref()
                .is_none_or(|previous| !Arc::ptr_eq(previous, baseline))
        });
        let mut historical_tools: HashMap<SharedString, HashSet<String>> = HashMap::new();
        let mut fully_historical: HashSet<SharedString> = HashSet::new();
        if baseline_changed {
            let baseline = baseline.as_ref().unwrap();
            let state = self.state.read(cx);
            let entries = match &self.doc_override {
                Some(id) => state.sub_transcript(id),
                None => &state.transcript,
            };
            let previous_markdown = std::mem::take(&mut self.historical_markdown);
            let prepared = self
                .chat_id
                .as_ref()
                .and_then(|id| state.prepared_transcripts.get(id));
            let mut historical_rows = Vec::new();
            for entry in entries {
                let covered = prepared.map_or_else(
                    || baseline.covers(entry),
                    |p| {
                        Arc::ptr_eq(baseline, &p.navigation_baseline)
                            || p.fully_historical.contains(&entry.id)
                    },
                );
                if covered {
                    fully_historical.insert(entry.id.clone().into());
                    continue;
                }
                if let Some(rows) = self
                        .chat_id
                        .as_ref()
                        .and_then(|id| state.prepared_transcripts.get(id))
                        .and_then(|p| p.historical.get(&entry.id))
                {
                    historical_rows.extend(rows.iter().cloned());
                    continue;
                }
                let Some(historical) = baseline.historical_entry(entry) else {
                    continue;
                };
                historical_rows.extend(rows_for_entry(
                    &historical,
                    false,
                    &mut |_, text| Arc::new(parse_full(text)),
                ));
            }
            // A normal opening snapshot is entirely historical. Share its
            // parsed trees instead of parsing the whole transcript twice;
            // only an entry mixing historical and live parts needs a prefix.
            historical_rows.extend(
                new_rows
                    .iter()
                    .filter(|row| {
                        fully_historical.contains(&row.entry_id)
                            && matches!(row.kind, RowKind::LiveMarkdown { .. })
                    })
                    .cloned(),
            );
            for row in historical_rows {
                match &row.kind {
                    RowKind::ToolGroup { tools, .. }
                        if !fully_historical.contains(&row.entry_id) =>
                    {
                        historical_tools
                            .entry(row.entry_id.clone())
                            .or_default()
                            .extend(tools.iter().map(|tool| tool.part_id.clone()));
                    }
                    RowKind::LiveMarkdown { .. } => {
                        if previous_markdown
                            .get(&row.id)
                            .is_none_or(|old| old.version != row.version)
                        {
                            self.veils.remove(&row.id);
                        }
                        self.historical_markdown.insert(row.id.clone(), row);
                    }
                    _ => {}
                }
            }
            self.last_replay_baseline = Some(baseline.clone());
        }

        // Give only rows that ARRIVE after the replay baseline an entrance.
        // The first populated frame after a chat attach may already contain a
        // live tool group; treating it as history prevents a whole existing
        // task tree from reanimating on every chat switch.
        let replay_baseline = self.veil_attach_pending && !entries_empty;
        if replay_baseline {
            // Retain explicit user pins, but never resume an old arrival or
            // closing animation when revisiting the retained transcript.
            self.tool_group_reveals.clear();
            for fold in self.folds.values_mut() {
                fold.toggled_at = None;
                fold.disclosure_at = None;
            }
        }
        let previous_tools: HashMap<SharedString, HashMap<String, Option<Instant>>> = self
            .rows
            .iter()
            .filter(|row| !fully_historical.contains(&row.entry_id))
            .filter_map(|row| match &row.kind {
                RowKind::ToolGroup { tools, .. } => {
                    let reveal = self.tool_group_reveals.get(&row.id);
                    Some((
                        row.id.clone(),
                        tools
                            .iter()
                            .enumerate()
                            .map(|(ix, tool)| {
                                (
                                    tool.part_id.clone(),
                                    reveal.and_then(|r| r.starts.get(ix).copied().flatten()),
                                )
                            })
                            .collect(),
                    ))
                }
                _ => None,
            })
            .collect();
        let now = crate::clock::now(cx);
        let mut live_tool_groups = HashSet::new();
        for row in &new_rows {
            let RowKind::ToolGroup { tools, .. } = &row.kind else {
                continue;
            };
            // Agent/spawn groups are standalone cards, not task trees.
            if !tool_group_collapses(tools) {
                continue;
            }
            live_tool_groups.insert(row.id.clone());
            let historical = historical_tools.get(&row.entry_id);
            let whole_group_historical =
                fully_historical.contains(&row.entry_id) || (replay_baseline && baseline.is_none());
            let is_historical = |tool: &ToolItem| {
                whole_group_historical || historical.is_some_and(|ids| ids.contains(&tool.part_id))
            };
            let historical_count = if whole_group_historical {
                tools.len()
            } else {
                tools.iter().filter(|tool| is_historical(tool)).count()
            };
            let previous = previous_tools.get(&row.id);
            let is_new_group = historical_count == 0 && previous.is_none();
            let reveal = self.tool_group_reveals.entry(row.id.clone()).or_default();
            if baseline_changed && historical_count > 0 {
                reveal.rendered_open = None;
                if let Some(fold) = self.folds.get_mut(&row.id) {
                    fold.toggled_at = None;
                    fold.disclosure_at = None;
                }
            }
            if is_new_group {
                reveal.header_started_at.get_or_insert(now);
            }
            if baseline_changed && historical_count == tools.len() {
                reveal.header_started_at = None;
            }
            let first_row_delay = is_new_group.then_some(TOOL_FIRST_ROW_DELAY_MS).unwrap_or(0);
            let mut arrival_ix = 0;
            if whole_group_historical {
                reveal.starts.clear();
                reveal.starts.resize(tools.len(), None);
                continue;
            }
            reveal.starts = tools
                .iter()
                .map(|tool| {
                    if is_historical(tool) {
                        return None;
                    }
                    if let Some(start) = previous.and_then(|tools| tools.get(&tool.part_id)) {
                        return *start;
                    }
                    let start = now
                        + Duration::from_millis(first_row_delay + arrival_ix * TOOL_ROW_STAGGER_MS);
                    arrival_ix += 1;
                    Some(start)
                })
                .collect();
        }
        self.tool_group_reveals
            .retain(|row_id, _| live_tool_groups.contains(row_id));

        // Runtime scroll handles follow the stable code rows exactly. A live
        // block keeps its handle through completion; deleted/reindexed tail
        // blocks and the previous chat cannot accumulate stale handles.
        let active_code_fences: HashSet<SharedString> = new_rows
            .iter()
            .flat_map(|row| match &row.kind {
                RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => {
                    tree.blocks
                        .get(*block_ix)
                        .map(|top| {
                            render::code_block_indices(&top.block, *block_ix)
                                .into_iter()
                                .map(|ix| format!("{}#code{ix}", row.id).into())
                                .collect()
                        })
                        .unwrap_or_default()
                }
                _ => Vec::new(),
            })
            .collect();
        self.code_fences
            .retain(|key, _| active_code_fences.contains(key));
        // Diagram source toggles follow the same stable row identity.
        if self.diagrams.borrow().has_source_toggles() {
            let row_ids: HashSet<&str> = new_rows.iter().map(|row| row.id.as_ref()).collect();
            self.diagrams.borrow_mut().retain_rows(&row_ids);
        }

        // Text already streamed before this (re)attach is the veil BASELINE:
        // its rows' veils seed instead of fading (render creates them from
        // this set), so only post-switch appends animate. Captured from the
        // first NON-EMPTY transcript after attach — the replay frame — never
        // the attach-time sync, whose transcript is still empty (selection
        // clears it; the doc watch refills it async).
        if self.veil_attach_pending
            && (!entries_empty || replay.authoritative_empty() || baseline_changed)
        {
            self.veil_attach_pending = false;
            self.veil_baseline = new_rows
                .iter()
                .filter(|r| baseline.is_none() && matches!(r.kind, RowKind::LiveMarkdown { .. }))
                .map(|r| r.id.clone())
                .collect();
        }

        // Veils live exactly as long as their live row — drop them on the
        // live→complete flip (any mid-fade chunk snaps to full, matching the
        // row's version splice).
        let active_markdown: HashSet<&SharedString> = new_rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::LiveMarkdown { .. }))
            .map(|row| &row.id)
            .collect();
        self.veils.retain(|id, _| active_markdown.contains(id));
        self.veil_baseline.retain(|id| active_markdown.contains(id));
        self.historical_markdown
            .retain(|id, _| active_markdown.contains(id));

        // slopty: not following, the first row in view holds its place by its key, whatever the
        // splice replaces around it (`inspector.md`, "Follow and anchor"). A list the layout
        // clamped to its end (its rows under the held one no longer filled the view) is held by
        // the row it shows first, so what lands below never carries it along.
        if !self.pinned && self.pending_viewport.is_none() && self.is_glued() {
            self.materialize_scroll_anchor();
        }
        let held = (!self.pinned && self.pending_viewport.is_none())
            .then(|| ViewportAnchor::capture(&self.rows, self.list.logical_scroll_top()))
            .flatten();
        let old_last = self.rows.len().checked_sub(1);
        match diff_rows(&self.rows, &new_rows) {
            None => {
                self.rows = new_rows;
                self.refresh_protected_attachments(cx);
                self.release_departed_tool_images(cx);
                // Replay readiness is independent of row content: an empty
                // reset (or one identical to optimistic rows) still resolves
                // or retires the pending viewport.
                if self.restore_pending_viewport(replay) {
                    cx.notify();
                }
                return;
            }
            Some((old_range, count)) => {
                // Any replaced row's cached flatten results are stale — and
                // because live replies splice only the rows whose content hash
                // changed (the tail), this is O(changed rows) per commit, never
                // O(reply).
                for row in &self.rows[old_range.clone()] {
                    let mut cache = self.render_cache.borrow_mut();
                    cache.invalidate_row(&row.id);
                    if matches!(row.kind, RowKind::User { .. }) {
                        cache.invalidate_row(&user_markdown_key(&row.id));
                    }
                }
                if old_range.len() == count {
                    // In-place content change, same row count — notably the
                    // live→complete flip, where EVERY row of the streamed
                    // message changes version (streaming bit, tool auto_open,
                    // timestamp bit) with identical ids. `splice` would reset
                    // those items to hint-less Unmeasured (heights read 0
                    // until the next paint) and, when the viewport-top item is
                    // inside the range, clobber the scroll anchor to the range
                    // start. `remeasure_items` keeps old sizes as hints
                    // and holds the anchor across the remeasure.
                    self.list.remeasure_items(old_range);
                } else {
                    self.list.splice(old_range, count);
                }
                self.viewport_layout_revision = self.viewport_layout_revision.wrapping_add(1);
            }
        }
        self.rows = new_rows;
        if old_last != self.rows.len().checked_sub(1) {
            if let Some(ix) = old_last.filter(|&ix| ix < self.rows.len()) {
                // Bottom chrome moves to the new tail too.
                self.list.remeasure_items(ix..ix + 1);
            }
        }
        if let Some(offset) = held.and_then(|held| held.resolve_exact(&self.rows))
            && offset != self.list.logical_scroll_top()
        {
            self.list.scroll_to(offset);
        }
        self.refresh_protected_attachments(cx);
        self.release_departed_tool_images(cx);
        self.restore_pending_viewport(replay);
        if self.land_end_pending && !self.rows.is_empty() {
            // First content for an unpinned override tab: land at the end.
            // `scroll_to_end` is ITEM-anchored (past-the-end offset that the
            // next layout materializes) — a pixel scroll off `max_offset`
            // would land short here, since the freshly-spliced rows are
            // still unmeasured. Short content clamps back to the top under
            // Top alignment, so "end" and "top" coincide there.
            self.land_end_pending = false;
            self.list.scroll_to_end();
        }
        // slopty: following, the list's tail mode keeps its end in view on every layout.
        cx.notify();
    }

    /// Cached row build for one entry (streaming entries bypass the cache).
    fn rows_for(&mut self, entry: &SessionMessageEntry, pending: bool) -> Vec<Row> {
        let streaming = entry.status == Some(MessageStatus::Streaming);
        // Live entries always rebuild; don't allocate a fingerprint that the
        // streaming path cannot use.
        let fingerprint = if streaming {
            0
        } else {
            entry_fingerprint(entry, pending)
        };
        let mut rows = if !streaming
            && let Some(cached) = self.row_cache.get(&entry.id)
            && cached.fingerprint == fingerprint
        {
            cached.rows.clone()
        } else {
            let live_parsers = &mut self.live_parsers;
            let tree_cache = &mut self.tree_cache;
            let mut parse = |key: &str, text: &str| -> Arc<BlockTree> {
                // Render-cache invalidation rides on the row diff in `sync` (only
                // rows whose content hash changed are spliced — the reparsed tail).
                parse_for_row(streaming, key, text, live_parsers, tree_cache).0
            };
            rows_for_entry(entry, pending, &mut parse)
        };

        if !streaming {
            self.row_cache.insert(
                entry.id.clone(),
                CachedRows {
                    fingerprint,
                    rows: rows.clone(),
                },
            );
        }
        rows.retain(|row| self.fold_shows(row)); // slopty: shared with the prepared rows.
        rows
    }

    /// A row shows unless its fold (a long plan's) is closed.
    fn fold_shows(&self, row: &Row) -> bool {
        row.compact_fold
            .as_ref()
            .is_none_or(|id| self.folds.get(id).is_some_and(|fold| fold.open == Some(true)))
    }

    /// Fetch a sidecar blob (full tool output or diff) and build its upgraded
    /// [`ToolDetail`] once, off the render path. Re-entry while Loading/Ready
    /// is a no-op; Failed re-arms as a retry (the affordance label says so).
    fn spawn_blob_fetch(&mut self, blob_ref: SharedString, cx: &mut Context<Self>) {
        // Rank BEFORE the already-fetched guard: clicking a Ready ref is the
        // "show me this one again" toggle (recency bump + repaint, no
        // re-fetch) — with both a diff and an output fetched, the two
        // affordances must be able to trade places forever.
        self.blob_fetch_counter += 1;
        self.blob_fetch_order
            .insert(blob_ref.clone(), self.blob_fetch_counter);
        match self.blob_details.get(&blob_ref) {
            Some(BlobFetch::Ready(_)) => {
                cx.notify();
                return;
            }
            Some(BlobFetch::Loading(_)) => return,
            Some(BlobFetch::Failed) | None => {}
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let is_diff = blob_ref.ends_with(".diff");
        let ref_key = blob_ref.clone();
        let task = cx.spawn(async move |this, cx| {
            let reply = crate::attachments::call_with_timeout(
                &engine,
                cx.background_executor(),
                slopty_view_rpc::methods::FETCH_TOOL_BLOB,
                serde_json::json!({ "blobRef": ref_key.as_ref() }),
                Duration::from_secs(20),
            )
            .await;
            let fetched = match reply {
                Ok(value) => {
                    let text = value
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default();
                    blob_detail(text, is_diff)
                        .map(|d| BlobFetch::Ready(Arc::new(d)))
                        .unwrap_or(BlobFetch::Failed)
                }
                Err(_) => BlobFetch::Failed,
            };
            this.update(cx, |this, cx| {
                this.blob_details.insert(ref_key, fetched);
                cx.notify();
            })
            .ok();
        });
        self.blob_details.insert(blob_ref, BlobFetch::Loading(task));
    }

    /// Expand/collapse one long user bubble. Heights come from the text's
    /// passive paint cache, never from transcript state, so this changes
    /// render-local fold state only and does not rebuild or splice rows.
    fn toggle_user_fold(
        &mut self,
        row_id: SharedString,
        row_ix: usize,
        collapsed_h: f32,
        full_h: f32,
        reduced_motion: bool,
        now: Instant,
    ) {
        let duration_ms = user_resize_duration_ms(full_h - collapsed_h);
        // A fold owns the viewport just like explicit navigation: following the end would hide
        // the beginning of the newly expanded prompt.
        self.begin_scroll_navigation();
        let entry = self.user_folds.entry(row_id).or_default();
        let currently_open = entry.open.unwrap_or(false);
        entry.from = if currently_open { full_h } else { collapsed_h };
        entry.open = Some(!currently_open);
        entry.epoch += 1;
        entry.toggled_at = Some(now);
        entry.duration_ms = duration_ms;
        entry.user_expansion_height = (full_h - collapsed_h).max(0.0);

        // Capture a screen-space anchor for the clicked row. We do not subtract
        // the removed height: that is only correct when the row's top is
        // exactly at the viewport bottom. At the top or middle of a long
        // message it overscrolls past the collapsed bubble. The same anchor is
        // used for expansion, with the full target height, so a bottom-pinned
        // prompt reveals its beginning below the top fade instead of above it.
        let Some(item_bounds) = self.list.bounds_for_item(row_ix) else {
            return;
        };
        let viewport = self.list.viewport_bounds();
        let initial_top = f32::from(item_bounds.top());
        // Keep a newly revealed bubble below the transcript's top fade band. A
        // 12px inset alone still leaves the first lines washed into the edge
        // fade when the expanded row started above view.
        let viewport_top = f32::from(viewport.top()) + Theme::TRANSCRIPT_FADE_BAND + 28.0;
        let target_height = if currently_open { collapsed_h } else { full_h };
        let viewport_bottom = f32::from(viewport.bottom()) - target_height - 12.0;
        let target_top = if viewport_bottom >= viewport_top {
            initial_top.clamp(viewport_top, viewport_bottom)
        } else {
            viewport_top
        };
        let needs_scroll = (target_top - initial_top).abs() > 0.5;

        if needs_scroll {
            if reduced_motion {
                if let Some(current) = self.list.bounds_for_item(row_ix) {
                    self.list
                        .scroll_by(px(f32::from(current.top()) - target_top));
                }
            } else {
                self.user_collapse_scroll = Some(UserCollapseScroll {
                    started_at: now,
                    duration_ms,
                    height_delta: (full_h - collapsed_h).max(0.0),
                    row_ix,
                    initial_top,
                    target_top,
                });
            }
        }
    }

    fn cancel_user_hold(&mut self) {
        self.user_hold_token = self.user_hold_token.wrapping_add(1);
        self.user_hold_task = None;
    }

    /// Arm a long-press toggle instead of using double-click. Releasing before
    /// the threshold preserves an ordinary click/selection gesture; moving
    /// cancels the timer so drag selection never unexpectedly toggles the
    /// message.
    fn arm_user_hold(
        &mut self,
        row_id: SharedString,
        row_ix: usize,
        collapsed_h: f32,
        measured_h: Rc<Cell<f32>>,
        selection_key: Arc<str>,
        cx: &mut Context<Self>,
    ) {
        const USER_HOLD_DELAY: Duration = Duration::from_millis(360);
        self.cancel_user_hold();
        self.user_hold_token = self.user_hold_token.wrapping_add(1);
        let token = self.user_hold_token;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(USER_HOLD_DELAY).await;
            this.update(cx, |this, cx| {
                if this.user_hold_token != token {
                    return;
                }
                this.user_hold_task = None;
                crate::markdown::selection::clear_if_owner(&selection_key);
                this.toggle_user_fold(
                    row_id,
                    row_ix,
                    collapsed_h,
                    measured_h.get().max(collapsed_h),
                    motion::reduced_motion(cx),
                    crate::clock::now(cx),
                );
                cx.notify();
            })
            .ok();
        });
        self.user_hold_task = Some(task);
    }

    fn step_user_collapse_scroll(&mut self, cx: &mut Context<Self>) {
        let Some(scroll) = self.user_collapse_scroll.as_ref() else {
            return;
        };
        let started_at = scroll.started_at;
        let duration_ms = scroll.duration_ms;
        let height_delta = scroll.height_delta;
        let row_ix = scroll.row_ix;
        let initial_top = scroll.initial_top;
        let target_top = scroll.target_top;
        let raw =
            (crate::clock::now(cx).saturating_duration_since(started_at).as_secs_f32() / (duration_ms as f32 / 1000.0)).clamp(0.0, 1.0);
        let spec = user_resize_spec(height_delta);
        let progress = spec.progress(raw);
        let desired_top = motion::lerp(initial_top, target_top, progress);
        if let Some(current) = self.list.bounds_for_item(row_ix) {
            // `scroll_by(+x)` moves content up, so correcting current minus
            // desired keeps the row on the interpolated screen-space path.
            let correction = f32::from(current.top()) - desired_top;
            if correction.abs() > 0.1 {
                self.list.scroll_by(px(correction));
            }
        }
        if raw >= 1.0 {
            self.user_collapse_scroll = None;
            self.last_scroll_distance = self.distance_from_bottom();
        }
        cx.notify();
    }

    /// slopty: open or shut the group `row_id`, as the person asked: the tween runs from the
    /// height it was drawn at (`open_height` open, none shut).
    fn toggle_fold(&mut self, row_id: SharedString, open_height: f32, cx: &mut Context<Self>) {
        let entry = self.folds.entry(row_id).or_default();
        let was_open = entry.open.unwrap_or(false);
        entry.from = if was_open { open_height } else { 0.0 };
        entry.open = Some(!was_open);
        entry.epoch += 1;
        entry.toggled_at = Some(crate::clock::now(cx));
        entry.disclosure_at = entry.toggled_at;
    }

    // ---- attachment read-back (user-attachments.tsx + transcript cache) ----

    /// Shield the open transcript's attachments from image-cache eviction —
    /// rebuilt on every row sync so a chat switch swaps the set. Without it,
    /// budget pressure evicted thumbnails still on screen (the list caches
    /// rendered rows, so a visible image's LRU tick goes stale).
    fn refresh_protected_attachments(&self, cx: &Context<Self>) {
        // The protected set is GLOBAL and replaced wholesale — an override
        // instance writing it would clobber the primary transcript's keys.
        if self.doc_override.is_some() {
            return;
        }
        crate::attachments::protect_attachments(self.protected_attachment_keys(cx));
    }

    fn protected_attachment_keys(&self, cx: &Context<Self>) -> HashSet<(String, String)> {
        let devices = self.attachment_device_ids(cx);
        let mut keys = std::collections::HashSet::new();
        for row in &self.rows {
            // Generated images use bounded LRU retention, not history-wide protection.
            if let RowKind::User { attachments, .. } = &row.kind {
                for path in attachments.iter().map(|att| att.path.clone()) {
                    for dev in &devices {
                        keys.insert((dev.clone(), path.clone()));
                    }
                }
            }
        }
        keys
    }

    /// Devices that may own a user message's attachment files: the chat's host
    /// device (uploads targeted it) plus this device (slopty's
    /// `uniqueIds([attachmentDeviceId, m.device_id])`).
    fn attachment_device_ids(&self, cx: &Context<Self>) -> Vec<String> {
        // `selected_chat_row` belongs to the PRIMARY transcript's chat — an
        // override instance has no chat row, so it claims no devices (its
        // thumbnails degrade to placeholders instead of guessing).
        if self.doc_override.is_some() {
            return Vec::new();
        }
        let state = self.state.read(cx);
        let mut ids = Vec::new();
        if let Some(chat) = state.selected_chat_row() {
            ids.push(chat.device_id.clone());
        }
        if let Some(local) = state.local_device_id.clone()
            && !ids.contains(&local)
        {
            ids.push(local);
        }
        ids
    }

    fn generated_attachment_device_ids(&self, owner: &str, cx: &Context<Self>) -> Vec<String> {
        let mut fallback = self.attachment_device_ids(cx);
        if let Some(local) = &self.state.read(cx).local_device_id {
            fallback.push(local.clone());
        }
        generated_image_devices(owner, &fallback)
    }

    /// Effective load state for one attachment across its candidate devices:
    /// first Loaded source wins; otherwise loads are (re)claimed and the
    /// snapshot degrades Loading → Error with a scheduled retry wake-up.
    fn attachment_state(
        &mut self,
        device_ids: &[String],
        path: &str,
        expected_raster_mime: Option<&str>,
        cx: &mut Context<Self>,
    ) -> crate::attachments::AttachmentSnapshot {
        use crate::attachments::{
            AttachmentKey, AttachmentSnapshot, attachment_snapshot_for, begin_load_for,
        };
        for dev in device_ids {
            if let AttachmentSnapshot::Loaded(image) =
                attachment_snapshot_for(&AttachmentKey::new(dev, path, expected_raster_mime))
            {
                return AttachmentSnapshot::Loaded(image);
            }
        }
        let mut any_loading = false;
        let mut min_retry: Option<Duration> = None;
        for dev in device_ids {
            if begin_load_for(&AttachmentKey::new(dev, path, expected_raster_mime)) {
                self.spawn_attachment_load(
                    dev.clone(),
                    path.to_string(),
                    expected_raster_mime.map(str::to_owned),
                    cx,
                );
            }
            match attachment_snapshot_for(&AttachmentKey::new(dev, path, expected_raster_mime)) {
                AttachmentSnapshot::Loaded(image) => return AttachmentSnapshot::Loaded(image),
                AttachmentSnapshot::Loading => {
                    // Generated assets try the owner first, falling back only
                    // after failure. Repainting never launches duplicate reads.
                    if expected_raster_mime.is_some() {
                        return AttachmentSnapshot::Loading;
                    }
                    any_loading = true;
                }
                AttachmentSnapshot::Error { retry_in } => {
                    min_retry = Some(min_retry.map_or(retry_in, |m| m.min(retry_in)));
                }
            }
        }
        if any_loading {
            return AttachmentSnapshot::Loading;
        }
        match min_retry {
            Some(retry_in) => {
                if let Some(dev) = device_ids.first() {
                    self.schedule_attachment_retry((dev.clone(), path.to_string()), retry_in, cx);
                }
                AttachmentSnapshot::Error { retry_in }
            }
            // No candidate devices at all — the "unavailable" thumb, no retry.
            None => AttachmentSnapshot::Error {
                retry_in: Duration::MAX,
            },
        }
    }

    fn spawn_attachment_load(
        &mut self,
        device_id: String,
        path: String,
        expected_raster_mime: Option<String>,
        cx: &mut Context<Self>,
    ) {
        use crate::attachments::{
            AttachmentKey, read_attachment_image, store_error_for, store_loaded_for,
        };
        let key = AttachmentKey::new(&device_id, &path, expected_raster_mime.as_deref());
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            store_error_for(&key);
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        // Relay-forward only for a genuinely remote owner; the local device's
        // files are served directly.
        let target = (local.as_deref() != Some(device_id.as_str())).then(|| device_id.clone());
        let claim = crate::attachments::AttachmentLoadGuard(key.clone());
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let _claim = claim;
            match read_attachment_image(
                &engine,
                cx.background_executor(),
                target.as_deref(),
                &path,
                expected_raster_mime.as_deref(),
            )
            .await
            {
                Some(loaded) => store_loaded_for(&task_key, loaded.name.into(), loaded.image),
                None => store_error_for(&task_key),
            }
            this.update(cx, |transcript, cx| {
                transcript.attachment_loads.remove(&task_key);
                cx.notify();
            })
            .ok();
        });
        self.attachment_loads.insert(key, task);
    }

    /// One wake-up per errored source: after the backoff elapses, a notify
    /// re-renders the thumb, whose `begin_load` then claims the retry.
    fn schedule_attachment_retry(
        &mut self,
        key: (String, String),
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        if delay == Duration::MAX || self.attachment_retries.contains_key(&key) {
            return;
        }
        let wake = key.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(delay + Duration::from_millis(60))
                .await;
            this.update(cx, |transcript, cx| {
                transcript.attachment_retries.remove(&wake);
                cx.notify();
            })
            .ok();
        });
        self.attachment_retries.insert(key, task);
    }

    // ---- tool-call image previews + the image context menu ----

    fn tool_images_of(this: &mut Self) -> &mut crate::tool_images::ToolImages {
        &mut this.tool_images
    }

    /// The chat whose agent produced these rows (a subagent tab borrows the
    /// selected chat's host and cwd).
    fn image_host(&self, cx: &gpui::App) -> (Option<String>, Option<String>) {
        let state = self.state.read(cx);
        let chat = self
            .chat_id
            .as_deref()
            .and_then(|id| state.chats.iter().find(|chat| chat.id == id))
            .or_else(|| state.selected_chat_row());
        (
            chat.map(|chat| chat.device_id.clone()),
            chat.and_then(|chat| chat.cwd.clone()),
        )
    }

    /// Hold the previews of every chip whose body is mounted; release the
    /// rest. Returns `(absolute path, thumb)` lists per chip index.
    fn hold_tool_images(
        &mut self,
        row_id: &SharedString,
        tools: &[ToolItem],
        opens: &[bool],
        folds: &[FoldState],
        cx: &mut Context<Self>,
    ) -> Vec<Option<Vec<(String, crate::tool_images::Thumb)>>> {
        if !tools.iter().any(chip_has_images) {
            return vec![None; tools.len()];
        }
        let (device, cwd) = self.image_host(cx);
        let loader = crate::tool_images::Loader {
            engine: self.state.read(cx).engine().cloned(),
            local_device: self.state.read(cx).local_device_id.clone(),
        };
        tools
            .iter()
            .enumerate()
            .map(|(ix, tool)| {
                if !chip_has_images(tool) {
                    return None;
                }
                let chip = SharedString::from(format!("{row_id}#d{ix}"));
                let tweening = folds[ix].epoch > 0
                    && folds[ix]
                        .toggled_at
                        .is_some_and(|at| crate::clock::now(cx).saturating_duration_since(at) < FOLD_TWEEN_WINDOW);
                if !opens[ix] && !tweening {
                    self.tool_images.release(&chip, Self::tool_images_of, cx);
                    return None;
                }
                let keys: Vec<_> = tool
                    .images
                    .iter()
                    .map(|path| crate::tool_images::ImageKey {
                        device: device.clone().unwrap_or_default(),
                        path: crate::tool_images::resolve_path(path, cwd.as_deref()),
                    })
                    .collect();
                let paths: Vec<String> = keys.iter().map(|key| key.path.clone()).collect();
                let loader = if device.is_some() {
                    &loader
                } else {
                    &crate::tool_images::Loader {
                        engine: None,
                        local_device: None,
                    }
                };
                let thumbs = self
                    .tool_images
                    .hold(&chip, keys, loader, Self::tool_images_of, cx);
                Some(paths.into_iter().zip(thumbs).collect())
            })
            .collect()
    }

    /// Rows left the transcript (compact fold settled, history rebuilt):
    /// their chips' previews start the release grace.
    fn release_departed_tool_images(&mut self, cx: &mut Context<Self>) {
        if self.tool_images.is_idle() {
            return;
        }
        let live: HashSet<&str> = self
            .rows
            .iter()
            // slopty: and markdown rows, which hold the images they name.
            .filter(|row| {
                matches!(
                    row.kind,
                    RowKind::ToolGroup { .. } | RowKind::Markdown { .. } | RowKind::LiveMarkdown { .. }
                )
            })
            .map(|row| row.id.as_ref())
            .collect();
        self.tool_images
            .retain_rows(|row| live.contains(row), Self::tool_images_of, cx);
    }

    fn render_tool_image_strip(
        &self,
        chip: &SharedString,
        images: Vec<(String, crate::tool_images::Thumb)>,
        collapses: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::tool_images::Thumb;
        let devices: Rc<[String]> = self.image_host(cx).0.into_iter().collect();
        let image_count = images.len();
        let frames = images.into_iter().enumerate().map(|(ix, (path, thumb))| {
            let name = SharedString::from(file_badge_name(&path).to_owned());
            let width = match &thumb {
                Thumb::Ready { aspect, .. } => {
                    (TOOL_IMAGE_HEIGHT * aspect).clamp(48.0, TOOL_IMAGE_MAX_WIDTH)
                }
                _ => TOOL_IMAGE_HEIGHT * 4.0 / 3.0,
            };
            let menu_devices = devices.clone();
            let frame = div()
                .id(SharedString::from(format!("{chip}-img{ix}")))
                .w(px(width))
                .h(px(TOOL_IMAGE_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.0))
                .overflow_hidden()
                .border_1()
                .border_color(crate::theme::hairline(0.07))
                .bg(crate::theme::ink(0.035))
                .role(gpui::Role::Image) // slopty: a picture to assistive tech.
                .aria_label(name.clone())
                .on_mouse_down(
                    gpui::MouseButton::Right,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_image_menu(
                            path.clone(),
                            menu_devices.to_vec(),
                            event.position,
                            window,
                            cx,
                        );
                    }),
                );
            match thumb {
                // Explicit dims: img layout honors intrinsic aspect over a
                // percent height (see the attachment strip).
                Thumb::Ready { image, .. } => frame.child(
                    img(image)
                        .w(px(width - 2.0))
                        .h(px(TOOL_IMAGE_HEIGHT - 2.0))
                        .rounded(px(7.0))
                        .object_fit(ObjectFit::Contain),
                ),
                Thumb::Loading { .. } => frame
                    .text_size(px(TOOL_TEXT_SIZE))
                    .text_color(theme.text_faint)
                    .child("Loading image…"),
                Thumb::Failed => frame
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(12.0))
                    .text_size(px(TOOL_TEXT_SIZE))
                    .text_color(theme.text_faint)
                    .child("Image unavailable")
                    .child(div().max_w_full().truncate().child(name)),
            }
        });
        div()
            .h(px(TOOL_IMAGE_STRIP_HEIGHT))
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(DETAIL_SEPARATOR))
                    .flex_none()
                    .when(!collapses, |line| line.bg(crate::theme::hairline(0.06))),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{chip}-images")))
                    // slopty: a call's pictures read as one gallery.
                    .when(image_count > 1, |strip| {
                        strip
                            .role(gpui::Role::Group)
                            .aria_label(SharedString::from(format!("{image_count} images")))
                    })
                    .h(px(TOOL_IMAGE_HEIGHT + 2.0 * TOOL_IMAGE_PAD))
                    .py(px(TOOL_IMAGE_PAD))
                    .when(!collapses, |strip| strip.px(px(TOOL_IMAGE_PAD)))
                    .flex()
                    .flex_row()
                    .gap(px(TOOL_IMAGE_PAD))
                    .overflow_x_scroll()
                    .children(frames),
            )
            .into_any_element()
    }

    /// Open the image menu at the pointer. `devices` are the candidate owners
    /// of `path`, tried in order when copying the image.
    fn open_image_menu(
        &mut self,
        path: String,
        devices: Vec<String>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.image_menu.open(ImageMenu {
            path,
            devices,
            position,
            active: None,
        });
        window.focus(&self.image_menu_focus, cx);
        cx.notify();
    }

    fn close_image_menu(&mut self, cx: &mut Context<Self>) {
        if self.image_menu.begin_close(crate::clock::now(cx)) {
            crate::popover::reap_popup(cx, |this: &mut Self| &mut this.image_menu);
            cx.notify();
        }
    }

    fn image_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        use crate::popover::MenuKey;
        let Some(menu) = self.image_menu.open_mut() else {
            return;
        };
        match crate::popover::classify_key(
            &event.keystroke.key,
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            MenuKey::Escape => self.close_image_menu(cx),
            MenuKey::Up => menu.active = crate::popover::menu_step(menu.active, 2, -1),
            MenuKey::Down => menu.active = crate::popover::menu_step(menu.active, 2, 1),
            MenuKey::Enter => {
                if let Some(action) = menu.active {
                    self.dispatch_image_menu(action, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn dispatch_image_menu(&mut self, action: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.image_menu.as_open() else {
            return;
        };
        let (path, devices) = (menu.path.clone(), menu.devices.clone());
        self.close_image_menu(cx);
        match action {
            0 => cx.write_to_clipboard(gpui::ClipboardItem::new_string(path)),
            1 => self.copy_image(path, devices, cx),
            _ => {}
        }
    }

    /// Copy the original file (not the preview) from its owning device.
    fn copy_image(&mut self, path: String, devices: Vec<String>, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        self.image_copy = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            for device in &devices {
                let target = (local.as_deref() != Some(device.as_str())).then_some(device.as_str());
                if let Some((_, mime, bytes)) = crate::attachments::read_attachment_bytes(
                    &engine, &executor, target, &path, None,
                )
                .await
                {
                    let format =
                        gpui::ImageFormat::from_mime_type(&mime).unwrap_or(gpui::ImageFormat::Png);
                    // SVG is markup an agent may have written: copy the parsed,
                    // re-serialized tree (no scripts or external resources),
                    // as previews render it.
                    let bytes = if format == gpui::ImageFormat::Svg {
                        let svg = executor.spawn(async move {
                            usvg::Tree::from_data(&bytes, &crate::image_media::svg_options())
                                .map(|tree| tree.to_string(&usvg::WriteOptions::default()))
                        });
                        let Ok(svg) = svg.await else {
                            return;
                        };
                        svg.into_bytes()
                    } else {
                        bytes
                    };
                    let image = gpui::Image::from_bytes(format, bytes);
                    this.update(cx, |this, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&image));
                        this.image_copy = None;
                    })
                    .ok();
                    return;
                }
            }
        }));
    }

    fn render_image_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.image_menu.get()?;
        let theme = Theme::of(cx).for_popup();
        let mut card = crate::popover::popover_card(&theme)
            .id("transcript-image-menu-card")
            .track_focus(&self.image_menu_focus)
            .role(gpui::Role::Menu)
            .w(px(190.0))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event, _, cx| this.image_menu_key(event, cx)))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_image_menu(cx)))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (index, (label, icon)) in [
            ("Copy path", crate::icons::COPY),
            ("Copy image", crate::icons::FILE_IMAGE),
        ]
        .into_iter()
        .enumerate()
        {
            let id = SharedString::from(format!("transcript-image-menu-{index}"));
            card = card.child(
                crate::popover::menu_row(&theme, menu.active == Some(index), id.clone())
                    .id(id)
                    .role(gpui::Role::MenuItem)
                    .aria_label(label)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.dispatch_image_menu(index, cx)),
                    )
                    .child(
                        crate::icons::icon(icon)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .child(label),
            );
        }
        Some(crate::popover::menu_at(
            "transcript-image-menu",
            menu.position,
            card.into_any_element(),
            self.image_menu.closing_at(crate::clock::now(cx)),
        ))
    }

    /// The inside of a user bubble: the prompt text, clipped to
    /// [`USER_COLLAPSED_LINES`] until expanded, plus the expander chevron for
    /// prompts past the cap. Returns the bubble's children in order.
    ///
    /// The collapsed form clips a normally-laid-out text element at exactly
    /// five line boxes. Do not use gpui's `line_clamp` here: on an auto-width
    /// flex item it answers intrinsic-width probes with the truncated layout,
    /// collapsing the bubble to min-content width (one character per line).
    /// A plain height clip preserves the original bubble width calculation and
    /// never feeds measured layout back into the virtualized list.
    fn render_user_body(
        &mut self,
        row_id: &SharedString,
        row_ix: usize,
        text: SharedString,
        markdown: Option<UserMarkdown>,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fold = self.user_folds.get(row_id).copied().unwrap_or_default();
        let expanded = fold.open.unwrap_or(false);
        let line_height =
            f32::from(crate::typography::ui_rems(USER_LINE_HEIGHT).to_pixels(window.rem_size()));
        let collapsed_text_h = USER_COLLAPSED_LINES as f32 * line_height;
        // slopty: no continuation line: the clipped text fades out over its last line (#6).
        let collapsed_h = collapsed_text_h;
        let measured_h = self
            .user_heights
            .entry(row_id.clone())
            .or_insert_with(|| Rc::new(Cell::new(0.0)))
            .clone();
        let measured = measured_h.get();
        let collapsible = text.lines().count() > USER_COLLAPSED_LINES
            || (measured > 0.0 && measured > collapsed_text_h + 0.5)
            || (measured == 0.0 && user_message_needs_collapse(&text));
        let full_h = measured_h.get().max(collapsed_h);
        if let Some(fold) = self.user_folds.get_mut(row_id) {
            // Wrapping can change with the window width while expanded.
            fold.user_expansion_height = (full_h - collapsed_h).max(0.0);
        }

        // Height motion uses the same ease-out curve as sidebars, tool folds,
        // and pane transitions, with duration scaled to travel distance. The
        // full text remains laid out behind the clip; only the viewport over it
        // changes, so glyph wrapping never shifts.
        let duration_ms = fold
            .duration_ms
            .max(user_resize_duration_ms(full_h - collapsed_h));
        let animating = collapsible
            && fold.epoch > 0
            && fold
                .toggled_at
                .is_some_and(|at| cx.frame_time().saturating_duration_since(at) < Duration::from_millis(duration_ms + 200))
            && !motion::reduced_motion(cx);
        // At rest folded, a long prompt's Markdown lays out only what fills
        // the fold; motion and the open bubble need all of it.
        let folded = collapsible && !expanded && !animating;

        let hold_key = row_id.clone();
        let hold_height = measured_h.clone();
        let hold_selection: Arc<str> = format!("{row_id}:u").into();
        let body = div()
            .id(SharedString::from(format!("{row_id}-body")))
            // A long press toggles instead of double-click. A normal release
            // remains available for text selection, and pointer movement
            // cancels the pending toggle before a drag can select text.
            .when(collapsible, |el| {
                let down_key = hold_key.clone();
                let down_height = hold_height.clone();
                let down_selection = hold_selection.clone();
                el.on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.arm_user_hold(
                            down_key.clone(),
                            row_ix,
                            collapsed_h,
                            down_height.clone(),
                            down_selection.clone(),
                            cx,
                        );
                    }),
                )
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, _| {
                        this.cancel_user_hold();
                    }),
                )
                .on_mouse_up_out(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, _| {
                        this.cancel_user_hold();
                    }),
                )
                .on_mouse_move(cx.listener(|this, _, _, _| {
                    this.cancel_user_hold();
                }))
            })
            .child(match markdown {
                Some(UserMarkdown {
                    folded: Some(part), ..
                }) if folded => {
                    // The part is shorter than the prompt: its height isn't
                    // the one the fold animates to.
                    self.render_user_markdown(row_id, &part, None, theme, window, cx)
                }
                Some(UserMarkdown { full, .. }) => self.render_user_markdown(
                    row_id,
                    &full,
                    Some(measured_h.clone()),
                    theme,
                    window,
                    cx,
                ),
                None => user_bubble_text(row_id, text, theme, measured_h.clone(), cx.entity_id()),
            });
        // slopty: the clipped text fades out over its last line, as the pane's edges do (#6).
        let faded = |clip: AnyElement| {
            crate::edge_fade::edge_faded(line_height, false, true, clip).into_any_element()
        };
        let body: AnyElement = if animating {
            let from = fold.from;
            let to = if expanded { full_h } else { collapsed_h };
            let resize = user_resize_spec(full_h - collapsed_h);
            let clip = div()
                .overflow_hidden()
                .child(body)
                .with_animation(
                    SharedString::from(format!("{row_id}-user-resize-{}", fold.epoch)),
                    resize.animation(),
                    move |el, t| el.h(px(motion::lerp(from, to, t).max(0.0))),
                )
                .into_any_element();
            if expanded { clip } else { faded(clip) }
        } else if collapsible && !expanded {
            faded(div().h(px(collapsed_text_h)).overflow_hidden().child(body).into_any_element())
        } else {
            body.into_any_element()
        };
        div()
            .relative()
            .child(body)
            .when(collapsible, |el| {
                el.child(self.render_user_expander(
                    row_id,
                    row_ix,
                    expanded,
                    collapsed_h,
                    measured_h,
                    theme,
                    cx,
                ))
            })
            .into_any_element()
    }

    /// The chat's checkout, which tells a workspace-file link from a web one.
    fn workspace_root(&self, cx: &gpui::App) -> Option<SharedString> {
        let state = self.state.read(cx);
        self.chat_id
            .as_deref()
            .and_then(|chat_id| state.chats.iter().find(|chat| chat.id == chat_id))
            .or_else(|| state.selected_chat_row())
            .and_then(|chat| chat.cwd.as_deref())
            .map(SharedString::from)
    }

    /// A prompt's Markdown, rendered like a reply's at the bubble's size: its
    /// own selection keys, links and fences. The canvas behind it records the
    /// full height for the fold, as [`user_bubble_text`] does for plain text.
    fn render_user_markdown(
        &mut self,
        row_id: &SharedString,
        tree: &Arc<BlockTree>,
        measured_h: Option<Rc<Cell<f32>>>,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_key = user_markdown_key(row_id);
        // Kept across frames like a reply's blocks; replacing the row drops it.
        self.rendered_rows.insert(row_key.clone());
        let link = self.link_ui(cx);
        let tree = self.inline_code_tree(tree, link.as_ref(), cx);
        let code = tree
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(ix, top)| self.code_uis_for(&row_key, &top.block, ix, cx))
            .flatten()
            .collect::<HashMap<_, _>>();
        let opts = RenderOptions {
            tasks: None,
            media: None,
            row_key: row_key.clone(),
            veil: None,
            cache: (!render_cache_disabled()).then(|| self.render_cache.clone()),
            now: cx.frame_time(), // slopty: the frame's moment
            copy: Some(self.copy_ui_for(&row_key, cx)),
            link,
            workspace_root: self.workspace_root(cx),
            code: (!code.is_empty()).then_some(code),
            run: self.run_ui(cx), // slopty: Run in terminal.
            code_menu: Some(self.code_menu_ui(cx)),
            body: (render::MD_TEXT_SIZE, USER_LINE_HEIGHT), // slopty: the bubble's own size
        };
        let highlight = self.code_highlight_for(&row_key, &tree, None, cx);
        let entity_id = cx.entity_id();
        div()
            .relative()
            .when_some(measured_h, |el, measured_h| {
                el.child(
                    canvas(
                        move |bounds, _, _| bounds.size.height,
                        move |_, height, _, cx| {
                            // Passive geometry cache, like the plain bubble's.
                            let height = f32::from(height);
                            if (measured_h.get() - height).abs() > 0.5 {
                                measured_h.set(height);
                                // slopty: told once this frame is drawn, as the plain bubble is.
                                cx.defer(move |cx| cx.notify(entity_id));
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .child(render::render_tree(&tree, &opts, theme, window, &|ix| {
                highlight.get(&ix).cloned().flatten()
            }))
            .into_any_element()
    }

    /// A plain text link aligned with the message's left edge, following the
    /// continuation ellipsis when collapsed. No pill, border, or button wash.
    fn render_user_expander(
        &mut self,
        row_id: &SharedString,
        row_ix: usize,
        expanded: bool,
        collapsed_h: f32,
        measured_h: Rc<Cell<f32>>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let toggle_key = row_id.clone();
        let glyph = if expanded {
            crate::icons::ALT_ARROW_UP
        } else {
            crate::icons::ALT_ARROW_DOWN
        };
        let label = if expanded { "Show less" } else { "Show more" };
        // slopty: a quiet 12 pt control under the words, not a line of them, which the keyboard
        // reaches (#6, #9).
        let button = crate::surface_chrome::key_stop(
            div().id(SharedString::from(format!("{row_id}-expander"))),
            theme,
        )
            .rounded(px(4.0))
            .group("user-message-toggle")
            .role(gpui::Role::Button)
            .aria_label(if expanded {
                "Collapse message"
            } else {
                "Expand message"
            })
            .aria_expanded(expanded)
            .flex()
            .items_center()
            .gap(px(4.0))
            .text_size(crate::typography::ui_rems(12.0))
            .line_height(crate::typography::ui_rems(USER_TOGGLE_LINE_HEIGHT))
            .text_color(theme.text_muted)
            .cursor_pointer()
            .hover(|s| s.text_color(theme.text))
            .child(label)
            .child(
                crate::icons::icon(glyph)
                    .size(px(12.0))
                    .text_color(theme.text_muted)
                    .group_hover("user-message-toggle", |s| s.text_color(theme.text)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_user_fold(
                    toggle_key.clone(),
                    row_ix,
                    collapsed_h,
                    measured_h.get().max(collapsed_h),
                    motion::reduced_motion(cx),
                    crate::clock::now(cx),
                );
                cx.notify();
            }));
        div()
            .mt(px(USER_TOGGLE_GAP))
            .flex()
            .items_start()
            .child(button)
            .into_any_element()
    }

    /// Media wiring for settled Markdown: Mermaid fences become diagrams
    /// once rendered and keep their source until then (or on failure).
    /// Inline images keep their ordinary chat rendering.
    fn diagram_media(&mut self, cx: &Context<Self>) -> render::MediaUi {
        let cache = self.diagrams.clone();
        let owner = cx.weak_entity();
        let images = self.md_images.clone();
        let chart = self.chart_media(cx); // slopty: chart fences draw as pages (M)
        let media = self.diagram_media.get_or_insert_with(|| render::MediaUi {
            chart: None,
            // slopty: a reply's images draw, read from the chat's folder.
            image: Some(Rc::new(move |image, id, theme| {
                md_images::element(&images, image, id, theme)
            })),
            diagram: Some(Rc::new(move |code, id, _theme| {
                let lookup = cache.borrow_mut().request(code, &id);
                match lookup {
                    mermaid_cache::Lookup::Pending => render::DiagramView::Source,
                    mermaid_cache::Lookup::Failed(reason) => render::DiagramView::Failed(reason),
                    mermaid_cache::Lookup::Ready(media) => {
                        let show_source = cache.borrow().source_visible(&id);
                        let body = if show_source {
                            gpui::Empty.into_any_element()
                        } else {
                            let open = owner.clone();
                            let source = media.clone();
                            crate::image_media::preview_element(
                                &media,
                                format!("{id}-image").into(),
                                move |window, cx| {
                                    let _ = open.update(cx, |this, cx| {
                                        this.open_diagram_preview(source.clone(), window, cx)
                                    });
                                },
                            )
                        };
                        let toggle = owner.clone();
                        render::DiagramView::Diagram(render::DiagramUi {
                            body,
                            show_source,
                            toggle_source: Rc::new(move |_, cx| {
                                let _ = toggle.update(cx, |this, cx| {
                                    this.diagrams.borrow_mut().toggle_source(&id);
                                    let row = mermaid_cache::frame_row(&id).to_owned();
                                    this.diagram_layout_changed(&[row.into()], cx);
                                });
                            }),
                        })
                    }
                }
            })),
        });
        media.chart = Some(chart); // slopty:
        media.clone()
    }

    /// Render requested diagrams one at a time off the UI thread. The loop
    /// picks its next source between renders, so rows that scrolled away in
    /// the meantime are skipped rather than queued.
    fn ensure_diagram_worker(&mut self, cx: &mut Context<Self>) {
        if self.diagram_worker.is_some() || !self.diagrams.borrow_mut().take_new_requests() {
            return;
        }
        self.diagram_worker = Some(cx.spawn(async move |this, cx| {
            loop {
                let job = this.update(cx, |this, cx| {
                    let job = this.diagrams.borrow_mut().next_job();
                    if job.is_none() {
                        this.diagram_worker = None;
                    }
                    job.map(|code| {
                        (
                            code,
                            crate::markdown::mermaid::Palette::from_theme(Theme::of(cx)),
                            crate::theme::style_generation(),
                        )
                    })
                });
                let Ok(Some((code, palette, style))) = job else {
                    return;
                };
                let source = code.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let svg = crate::markdown::mermaid::render(&source, &palette)?;
                        crate::image_media::decode_image("image/svg+xml", svg.into_bytes())
                    })
                    .await;
                if this
                    .update(cx, |this, cx| this.finish_diagram(code, style, result, cx))
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn finish_diagram(
        &mut self,
        code: String,
        style: u32,
        result: Result<crate::image_media::MediaImage, String>,
        cx: &mut Context<Self>,
    ) {
        let Some((rows, released)) = self.diagrams.borrow_mut().finish(code, style, result) else {
            return;
        };
        crate::image_media::release_media(released, cx);
        self.diagram_layout_changed(&rows, cx);
    }

    /// A diagram replaced its source (or the reverse): remeasure the rows
    /// painting it and let the follow rule absorb the height change, exactly
    /// like any other layout-affecting row update.
    fn diagram_layout_changed(&mut self, rows: &[SharedString], cx: &mut Context<Self>) {
        let mut changed = false;
        for (ix, row) in self.rows.iter().enumerate() {
            if rows.contains(&row.id) {
                self.list.remeasure_items(ix..ix + 1);
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.viewport_layout_revision = self.viewport_layout_revision.wrapping_add(1);
        cx.notify();
    }

    /// Open a diagram in the shared lightbox, rasterized for the window
    /// within the memory the retained diagrams leave available.
    fn open_diagram_preview(
        &mut self,
        source: crate::image_media::MediaImage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_diagram_zoom(cx);
        let viewport = window.viewport_size();
        let available = mermaid_cache::MAX_RETAINED_BYTES
            .saturating_sub(self.diagrams.borrow().retained_bytes());
        let enlarged = source.enlarged(
            (
                f32::from(viewport.width) * 0.9,
                f32::from(viewport.height) * 0.85,
            ),
            window.scale_factor(),
            available,
            None,
        );
        self.attachment_preview_return_focus = window.focused(cx);
        self.attachment_preview = Some(
            crate::attachments::PreviewImage::new("Mermaid diagram", enlarged.image)
                .with_plate(crate::markdown::mermaid::Palette::plate(Theme::of(cx))),
        );
        self.diagram_zoom = Some(source);
        window.focus(&self.attachment_preview_focus, cx);
        cx.notify();
    }

    /// Release a diagram lightbox's dedicated raster. The row's own preview
    /// stays with the diagram cache.
    fn close_diagram_zoom(&mut self, cx: &mut gpui::App) {
        let Some(source) = self.diagram_zoom.take() else {
            return;
        };
        if let Some(preview) = self.attachment_preview.take()
            && !Arc::ptr_eq(&preview.image, &source.image)
        {
            cx.defer(move |cx| gpui::ImageSource::Image(preview.image).evict(None, cx));
        }
    }

    fn render_generated_image(
        &mut self,
        row_id: &SharedString,
        owner: &str,
        path: &str,
        name: &str,
        mime_type: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::attachments::AttachmentSnapshot;
        let devices = self.generated_attachment_device_ids(owner, cx);
        let state = self.attachment_state(&devices, path, Some(mime_type), cx);
        let theme = Theme::of(cx).clone();
        let frame = div()
            .id(SharedString::from(format!("{row_id}-generated")))
            .w(px(512.0))
            .max_w_full()
            .h(px(320.0))
            .max_h(px(420.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(TRANSCRIPT_CARD_RADIUS)) // slopty: the column's card (#19).
            .overflow_hidden()
            .bg(crate::theme::ink(0.045));
        match state {
            AttachmentSnapshot::Loaded(loaded) => {
                let dimensions =
                    crate::attachments::png_dimensions(&loaded.image.bytes).unwrap_or((512, 320));
                let scale = (512.0 / dimensions.0 as f32)
                    .min(420.0 / dimensions.1 as f32)
                    .min(1.0);
                let preview =
                    crate::attachments::PreviewImage::new(name.to_owned(), loaded.image.clone());
                let menu_path = path.to_owned();
                frame
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_image_menu(
                                menu_path.clone(),
                                devices.clone(),
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                    .w(px(dimensions.0 as f32 * scale))
                    // slopty: as tall as the picture at the width its column leaves it (the
                    // inspector's is narrower than 512), never a letterbox round it.
                    .h_auto()
                    .role(gpui::Role::Button)
                    .aria_label(image_label(path, name))
                    .tab_index(0)
                    .cursor_pointer()
                    // slopty: the shared ring, outside the picture: focus covers none of it.
                    .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.attachment_preview_return_focus = window.focused(cx);
                        preview.viewer.reset();
                        this.attachment_preview = Some(preview.clone());
                        window.focus(&this.attachment_preview_focus, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w_full()
                            .aspect_ratio(dimensions.0.max(1) as f32 / dimensions.1.max(1) as f32)
                            .child(
                                gpui::img(loaded.image)
                                    .size_full()
                                    // The frame's overflow clip is rectangular; round the image
                                    // itself.
                                    .rounded(px(TRANSCRIPT_CARD_RADIUS))
                                    .object_fit(gpui::ObjectFit::Contain),
                            ),
                    )
                    .into_any_element()
            }
            AttachmentSnapshot::Loading => frame
                .text_color(theme.text_muted)
                .child("Loading generated image…")
                .into_any_element(),
            AttachmentSnapshot::Error { .. } => frame
                .text_color(theme.text_muted)
                .child("Generated image unavailable")
                .into_any_element(),
        }
    }

    fn render_visual(
        &mut self,
        row_id: &SharedString,
        owner: &str,
        path: &str,
        title: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let link_ui = self.link_ui(cx);
        let frame = match self.visuals.get(row_id) {
            Some(frame) => frame.clone(),
            None => {
                let state = self.state.read(cx);
                let engine = state.engine().cloned();
                // Relay-forward only for a genuinely remote owner.
                let target =
                    (state.local_device_id.as_deref() != Some(owner)).then(|| owner.to_owned());
                let frame = cx.new(|cx| {
                    crate::visuals::VisualFrame::new(
                        row_id.clone(),
                        title.to_owned(),
                        path.to_owned(),
                        engine,
                        target,
                        cx,
                    )
                });
                // A visual whose tree changes shape (a tab switch, a load)
                // resizes its row, like a diagram finishing.
                let id = row_id.clone();
                cx.subscribe(
                    &frame,
                    move |this, _, _: &crate::visuals::VisualLayoutChanged, cx| {
                        this.diagram_layout_changed(std::slice::from_ref(&id), cx);
                    },
                )
                .detach();
                self.visuals.insert(row_id.clone(), frame.clone());
                frame
            }
        };
        frame.update(cx, |frame, _| frame.set_link_ui(link_ui));
        div().w_full().child(frame).into_any_element()
    }

    /// The right-aligned thumbnail strip above a user bubble.
    fn render_user_attachments(
        &mut self,
        row_id: &SharedString,
        atts: &[crate::attachments::UserImageAttachment],
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::attachments::AttachmentSnapshot;
        let device_ids = self.attachment_device_ids(cx);
        let mut strip = div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_end()
            .items_start()
            .gap(px(8.0))
            .px(px(4.0))
            .pt(px(4.0))
            .pb(px(6.0));
        for (aix, att) in atts.iter().enumerate() {
            if !crate::attachments::is_image_path(&att.path) {
                strip = strip.child(user_file_pill(&att.name, Theme::of(cx)));
                continue;
            }
            let state = self.attachment_state(&device_ids, &att.path, None, cx);
            // slopty: appshots removed.
            let frame = div()
                .flex_none()
                .w(px(ATT_THUMB_W))
                .h(px(ATT_THUMB_H))
                .rounded(px(8.0))
                .overflow_hidden();
            let thumb: AnyElement = match state {
                AttachmentSnapshot::Loaded(image) => {
                    let preview = crate::attachments::PreviewImage::new(
                        image.name.clone(),
                        image.image.clone(),
                    );
                    let (menu_path, menu_devices) = (att.path.clone(), device_ids.clone());
                    frame
                        .id(SharedString::from(format!("{row_id}#att{aix}")))
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_image_menu(
                                    menu_path.clone(),
                                    menu_devices.clone(),
                                    event.position,
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .relative()
                        .border_1()
                        .border_color(crate::theme::hairline(0.11))
                        .bg(crate::theme::ink(0.035))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.attachment_preview_return_focus = window.focused(cx);
                            preview.viewer.reset();
                            this.attachment_preview = Some(preview.clone());
                            window.focus(&this.attachment_preview_focus, cx);
                            cx.notify();
                        }))
                        .child(
                            img(image.image.clone())
                                // EXPLICIT dims, not size_full: img layout
                                // honors the intrinsic aspect ratio over a
                                // percent height (gpui f8d8a90 repoint), so
                                // size_full let a tall photo grow past the
                                // frame and the rectangular overflow clip
                                // squared the bottom corners (2026-08-19).
                                .w(px(ATT_THUMB_W - 2.0))
                                .h(px(ATT_THUMB_H - 2.0))
                                // The IMG needs its own radii: the frame's
                                // rounding only clips rectangularly, so the
                                // sprite must round its own corners (7 = the
                                // frame's 8 minus its 1px border).
                                .rounded(px(7.0))
                                .object_fit(ObjectFit::Cover),
                        )
                        .into_any_element()
                }
                // Errored/unavailable: the dashed "missing" thumb.
                AttachmentSnapshot::Error { .. } => frame
                    .border_1()
                    .border_dashed()
                    .border_color(crate::theme::hairline(0.14))
                    .bg(crate::theme::ink(0.025))
                    .into_any_element(),
                // Loading: the pulsing skeleton (same wash as popover skeletons).
                AttachmentSnapshot::Loading => frame
                    .border_1()
                    .border_color(crate::theme::hairline(0.08))
                    .bg(crate::theme::ink(0.055))
                    .opacity(
                        0.35 + 0.4
                            * motion::pulse_wave(motion::pulse_delta(
                                &motion::SLOPTY_PULSE,
                                cx.entity_id(),
                                cx,
                            )),
                    )
                    .into_any_element(),
            };
            strip = strip.child(thumb);
        }
        strip.into_any_element()
    }

    // ---- rendering ----

    /// The working loader, INSIDE the conversation flow: appended under the
    /// last row while the run is live (moved out of the shell's status strip
    /// — user request), so it reads as part of the streaming reply and
    /// scrolls away with it. The spinner drives this entity's frames, which
    /// keeps the elapsed timer ticking through delta-quiet tool runs.
    /// slopty: take the time again at the next whole second after `start` (ms since the epoch),
    /// or in a second, and draw again: what the working line says moves on only then.
    fn tick_clock(&mut self, start: Option<i64>, cx: &mut Context<Self>) {
        if self.clock_tick.is_some() {
            return;
        }
        let real = crate::clock::utc(cx).timestamp_millis();
        let wait = start.map_or(1000, |start| 1000 - (real - start).rem_euclid(1000));
        let wait = Duration::from_millis(u64::try_from(wait).unwrap_or(1000).clamp(1, 1000) + 2);
        self.clock_tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                this.clock_tick = None;
                this.clock = crate::clock::utc(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_working_trailer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let now = self.clock;
        // `turn` (the turn's start, ms) keys the rolling word and timer, so a
        // new turn's labels appear fresh instead of rolling from the last
        // turn's final values.
        let (elapsed_secs, seed, turn) = if let Some(doc_id) = &self.doc_override {
            // A subagent doc has no Session row — `indicator_for` would read
            // the PARENT chat's live state into this tab. Liveness rides the
            // doc itself instead: the sink's assistant entry streams until
            // the subagent settles (run teardown finalizes abandoned sinks),
            // and a trailing USER entry is a steer still awaiting its reply
            // segment. Frozen snapshots never spin, whatever they claim.
            if !self.doc_live {
                return None;
            }
            let state = self.state.read(cx);
            let last = state.sub_transcript(doc_id).last()?;
            let live =
                last.status == Some(MessageStatus::Streaming) || last.role == MessageRole::User;
            if !live {
                return None;
            }
            let elapsed = ((now.timestamp_millis() - last.created_at).max(0) / 1000) as i64;
            (elapsed, flavour_seed(doc_id), last.created_at)
        } else {
            let chat_id = self.chat_id.clone()?;
            let (elapsed, turn) = {
                let state = self.state.read(cx);
                if state.indicator_for(&chat_id, now) != crate::state::Indicator::Working {
                    return None;
                }
                let turn_started = state.session_for(&chat_id).and_then(|s| s.started_at);
                let elapsed = turn_started
                    .map(|t| now.signed_duration_since(t).num_seconds().max(0))
                    .unwrap_or(0);
                (elapsed, turn_started.map_or(0, |t| t.timestamp_millis()))
            };
            (elapsed, flavour_seed(&chat_id), turn)
        };
        // slopty: the clock moves on at the timer's next whole second, so the second a frame
        // says is the one every frame built after it says.
        self.tick_clock(Some(turn), cx);
        // slopty: the call under way, or the newest thought, in place of the flavour word.
        let step = {
            let state = self.state.read(cx);
            let entries = match &self.doc_override {
                Some(doc_id) => state.sub_transcript(doc_id),
                None => state.transcript.as_slice(),
            };
            entries.last().and_then(extras::working_step)
        };
        let word = if let Some(step) = step.as_deref() {
            step
        } else {
            flavour_word(seed, elapsed_secs)
        };
        let theme = Theme::of(cx).clone();
        let spoken = word.to_owned();
        Some(
            div()
                // slopty: the working line says what it shows: the step, or the word.
                .id("working-line")
                .role(gpui::Role::Status)
                .aria_label(SharedString::from(spoken))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                // slopty: the row lane's anatomy (`ROW_LANE_HEIGHT`).
                .pt(px(Theme::SPACE_SM))
                .h(px(ROW_LANE_HEIGHT))
                .text_size(crate::typography::ui_rems(WORKING_FIGURES_SIZE))
                .child(crate::loaders::gradient_spinner(
                    "working-indicator",
                    &theme,
                    2.5,
                    cx.entity_id(),
                    cx,
                ))
                .child(
                    // The flavour word rotates every 7s; roll it as it turns.
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(WORKING_LEAD_SIZE))
                        .text_color(theme.text_muted)
                        .child(crate::roll_text::roll_text(
                            format!("working-word-{}-{turn}", cx.entity_id()),
                            SharedString::from(format!("{word}…")),
                            cx.reduce_motion(),
                            crate::clock::now(cx),
                        )),
                )
                // The timer ticks every second; its digits roll in place (the unit suffix
                // holds).
                .child(
                    div()
                        .flex_none()
                        .relative()
                        .top(px(1.0))
                        .text_color(theme.text_muted)
                        .font_features(crate::typography::tabular())
                        .child(crate::roll_text::roll_text(
                            format!("working-timer-{}-{turn}", cx.entity_id()),
                            SharedString::from(ticking_elapsed(elapsed_secs)),
                            cx.reduce_motion(),
                            crate::clock::now(cx),
                        )),
                )
                .into_any_element(),
        )
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return gpui::Empty.into_any_element();
        };
        self.rendered_rows.insert(row.id.clone());
        let theme = Theme::of(cx).clone();
        // slopty: a subagent's words to start it are its task, which its parent's agent wrote.
        let (workspace_root, task, agent) = {
            let state = self.state.read(cx);
            let chat = self
                .chat_id
                .as_deref()
                .and_then(|chat_id| state.chats.iter().find(|chat| chat.id == chat_id))
                .or_else(|| state.selected_chat_row());
            (
                chat.and_then(|chat| chat.cwd.as_deref()).map(SharedString::from),
                self.fit.is_some() || chat.is_some_and(|chat| chat.subagent),
                // slopty: a reply is said by the chat's agent, by its harness's name (#10).
                agent_name(chat.and_then(|chat| chat.config.as_ref()).map(|config| config.harness)),
            )
        };
        // slopty: the transcript lays out inside its pane, which clears the window's chrome
        // itself: the first row keeps only the ordinary gap under the pane's header.
        let top_gap = if ix == 0 {
            if self.fit.is_some() {
                Theme::SPACE_SM // slopty: in a card, under its header's hairline
            } else {
                Theme::SPACE_LG
            }
        } else {
            top_gap_for(ix.checked_sub(1).and_then(|i| self.rows.get(i)), &row)
        };
        // The last row must clear the fade band at the bottom, or the timestamp strip
        // (the row's lowest content) renders half-faded (or hidden) when the
        // transcript is pinned to the bottom.
        // slopty: no composer stack under it to clear as well.
        let is_last = ix + 1 == self.rows.len();
        let bottom_pad = if is_last && self.fit.is_some() {
            // slopty: fit to its rows, nothing scrolls under it and no fade covers its end.
            Theme::SPACE_SM
        } else if is_last {
            Theme::TRANSCRIPT_FADE_BAND + 8.0
        } else {
            0.0
        };
        // Live-run loader rides under the LAST row's content (above its
        // clearance pad), so it sits right beneath the working reply.
        let trailer = (ix + 1 == self.rows.len())
            .then(|| self.render_working_trailer(cx))
            .flatten();

        // Hover-revealed metadata strip: a RESERVED 32px lane under the
        // entry's last row. Timestamp, copy action, and copied feedback only
        // flip visibility/content, so none of them shifts the virtualizer.
        // User entries align end (under the bubble), assistant entries start.
        // Both read timestamp first, then the copy action.
        let is_user_row = matches!(row.kind, RowKind::User { .. });
        let hovered = self
            .hovered_entry
            .as_ref()
            .is_some_and(|(_, entry)| entry == &row.entry_id)
            // slopty: touch has no hover, so every strip is there at rest, as opencode gates each
            // reveal on a hover the screen has (`.research/opencode-study-2026-10-19.md` item 4).
            || TOUCH;
        // slopty: the keyboard reaches the strip too: its controls stay mounted, unseen, and one
        // holding focus shows the strip as a hover does (#9); only a row with a strip has them.
        let copy_focus = row.timestamp.map(|_| self.strip_focus(&row.entry_id, window, cx));
        let shown = hovered || copy_focus.as_ref().is_some_and(|f| f.is_focused(window));
        // slopty: the strip fades in and out over 200 ms as the pointer comes and goes (Amp's
        // message actions; the Amp study's item 9).
        let (strip_t, strip_moving) = if row.timestamp.is_some() {
            self.strip_fade(&row.id, shown, window.frame_time(), motion::reduced_motion(cx))
        } else {
            (0.0, false)
        };
        if strip_moving {
            window.request_animation_frame();
        }
        // slopty: the house's copy span (`motion::copy_feedback`).
        let copied_message =
            motion::copied(&copy_message_key(&row.entry_id), cx);
        let copy_text = row.copy_text.clone();
        let copy_entry_id = row.entry_id.clone();
        let now_ms = self.clock.timestamp_millis();
        // slopty: under a pointer, the person's strip waits beside the bubble's foot and takes no
        // room under it, so their answer starts right under their question (#1). A touch screen
        // keeps the lane, where the strip shows without a hover; an image-only send keeps it too.
        let beside_bubble = !TOUCH
            && !task
            && matches!(&row.kind, RowKind::User { text, .. } if !text.is_empty());
        // slopty: a turn's done line says when it ended, so its Copy rides at the end of its
        // words, not alone in a lane under them (#7).
        let on_done_line = is_done_line(&row);
        // slopty: a run in its card shows its work, not its messages' times and actions; a run's
        // task, which the person never sent, has none either.
        let strip = row
            .timestamp
            .filter(|_| self.fit.is_none() && !(is_user_row && task))
            .map(|ms| {
            // slopty: the day only when it is not today, the full moment on hover (#17); worded
            // only while the strip shows, and the full moment only once its tooltip opens (#13).
            let timestamp = (ms > 0 && strip_t > 0.0).then(|| {
                div()
                    .id(SharedString::from(format!("{}-time", row.id)))
                    .flex_none()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_faint)
                    // A tooltip, not a name: the strip's words for assistive tech are its
                    // controls'.
                    .tooltip(move |window, cx| {
                        crate::settings::widgets::text_tooltip(format_full_timestamp(ms, &chrono::Local))(
                            window, cx,
                        )
                    })
                    .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                    .child(SharedString::from(format_timestamp_near(ms, now_ms, &chrono::Local)))
            });
            let copy = copy_text.map(|text| {
                let entry_id = copy_entry_id.clone();
                let glyph_key = copy_message_key(&entry_id);
                let fade_key = format!("copy-message-hover-{entry_id}");
                // slopty: a finger's press on a touch screen, the wash drawn on the 24 (#23).
                let (press, reach) = strip_press(format!("copy-message-{entry_id}"));
                let press = match &copy_focus {
                    Some(focus) => press.track_focus(focus), // slopty: a tab stop.
                    None => press,
                };
                strip_control(press, &theme, reach)
                    .cursor_pointer()
                    .hover_fade(fade_key.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_message(entry_id.clone(), text.clone(), cx)
                    }))
                    // slopty: keeps its size; its tooltip says "Copied" for the span; a role.
                    .role(gpui::Role::Button)
                    .hint(if copied_message { "Copied" } else { "Copy message" })
                    .child(
                        strip_drawn()
                            // Same quiet icon-button treatment as the copy action
                            // over transcript code blocks.
                            .bg(motion::hover_blend(
                                &fade_key,
                                gpui::transparent_black(),
                                crate::theme::ink(0.08),
                            ))
                            // slopty: its check drawn in as the copy lands (`CopyGlyph`).
                            .child(crate::loaders::copy_glyph::CopyGlyph::new(
                                glyph_key,
                                14.0,
                                theme.text_muted,
                            )),
                    )
            });
            let metadata = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM));
            // slopty: a time the agent never said (0) shows none; the copy action stays.
            let metadata = metadata.children(timestamp).children(copy);
            // slopty: TUI-first: the TUI goes back to a message (`/rewind`), not the record.
            // slopty: unseen until hovered or focused, but there for the keyboard (#9).
            let metadata = metadata.opacity(strip_t).into_any_element();
            if on_done_line {
                return div().flex_none().child(metadata).into_any_element();
            }
            if beside_bubble {
                // slopty: at the bubble's foot, to its left, drawn over the gutter (#1).
                return div()
                    .absolute()
                    .bottom_0()
                    .right(gpui::relative(1.0))
                    .h(px(STRIP_CONTROL_SIZE))
                    .pr(px(Theme::SPACE_SM))
                    .flex()
                    .items_center()
                    .child(metadata)
                    .into_any_element();
            }
            div()
                .h(px(ROW_LANE_HEIGHT))
                .pt(px(Theme::SPACE_SM))
                .w_full()
                .flex()
                .items_center()
                // No horizontal inset: the original's `px-1` netted out flush
                // because its message text was inset by the same amount (group
                // padding 4 + inner VStack 4 = 8 = group 4 + px-1 4). Here the
                // markdown text / user bubble sit AT the content column edges,
                // so the label must too — assistant label's left edge on the
                // text's first-character x, user label's right edge on the
                // bubble's right edge (user-reported 4px drift).
                .when(is_user_row && !task, |el| el.justify_end()) // slopty: a task reads left.
                .child(metadata)
                .into_any_element()
        });
        let (mut beside, strip) = if beside_bubble { (strip, None) } else { (None, strip) };
        let (mut done_trailing, strip) = if on_done_line { (strip, None) } else { (None, strip) };

        let inner: AnyElement = match &row.kind {
            RowKind::User {
                text,
                markdown,
                attachments,
                pending,
            } => {
                let attachments = attachments.clone();
                let text = text.clone();
                let markdown = markdown.clone();
                let pending = *pending;
                // Attachment thumbnails ride ABOVE the bubble, right-aligned
                // (chat-view.tsx RowView: UserAttachmentStrip then the text
                // HStack); image-only sends show no bubble at all.
                let mut column = div().w_full().flex().flex_col();
                if !attachments.is_empty() {
                    column = column.child(self.render_user_attachments(
                        &row.id,
                        &attachments,
                        window,
                        cx,
                    ));
                }
                // slopty: a run's first words are its task, which its parent's agent wrote: a
                // quiet paragraph, not the person's bubble, cut short in its card.
                if !text.is_empty() && task {
                    let lines = self.fit.is_some().then_some(subagents::TASK_LINES);
                    column = column.child(subagents::task_words(&row.id, &text, lines, &theme));
                } else if !text.is_empty() {
                    // `min_w_0` is load-bearing: gpui text answers min/max-content
                    // probes with its UNWRAPPED width, so without it the bubble's
                    // automatic min-size is the full single-line width — the flex
                    // item can't shrink, `justify_end` pushes the overflow off the
                    // left edge, and long prompts render as one clipped line
                    // instead of wrapping inside the 80% column cap.
                    // slopty: the person's own words are in the accessibility tree, as the
                    // agent's are: the bubble is a paragraph saying them.
                    let spoken = text.clone();
                    let bubble = div()
                                .id(SharedString::from(format!("{}-said", row.id)))
                                .role(gpui::Role::Paragraph)
                                .aria_label(spoken)
                                .min_w_0()
                                .max_w(px(self.content_width * 0.8))
                                .bg(queued_fill(
                                    crate::theme::user_bubble_bg(),
                                    theme.accent,
                                    if pending { QUEUED_TINT } else { 0.0 },
                                ))
                                .rounded(px(Theme::BUBBLE_RADIUS))
                                .px(px(16.0))
                                .py(px(10.0))
                                .text_size(crate::typography::ui_rems(14.0))
                                .line_height(crate::typography::ui_rems(USER_LINE_HEIGHT))
                                .text_color(theme.text)
                                // slopty: lit faintly in the theme's hues, under the words (Amp).
                                .relative()
                                .children(crate::lit::light_over(px(Theme::BUBBLE_RADIUS), &theme))
                                .child(self.render_user_body(
                                    &row.id, ix, text, markdown, &theme, window, cx,
                                ));
                    // slopty: a send the agent took loses its tint, not in one frame (#15).
                    let bubble = if self.bubble_sent(&row.id, pending, cx) {
                        bubble
                            .with_animation(
                                SharedString::from(format!("sent-{}", row.id)),
                                QUEUED_SETTLE.animation(),
                                {
                                    let accent = theme.accent;
                                    move |bubble, t| {
                                        bubble.bg(queued_fill(
                                            crate::theme::user_bubble_bg(),
                                            accent,
                                            QUEUED_TINT * (1.0 - t),
                                        ))
                                    }
                                },
                            )
                            .into_any_element()
                    } else {
                        bubble.into_any_element()
                    };
                    // slopty: the strip waits at the bubble's foot (#1).
                    let foot = div()
                        .relative()
                        .min_w_0()
                        .flex()
                        .child(bubble)
                        .children(beside.take());
                    column = column.child(div().w_full().flex().justify_end().child(foot));
                }
                column.into_any_element()
            }
            RowKind::Markdown { tree, block_ix } => {
                let link = self.link_ui(cx);
                let tree = self.inline_code_tree(tree, link.as_ref(), cx);
                let tree = self.chat_chips(&row.id, tree, cx); // slopty
                let Some(top) = tree.blocks.get(*block_ix) else {
                    return gpui::Empty.into_any_element();
                };
                let code = self.code_uis_for(&row.id, &top.block, *block_ix, cx);
                let opts = RenderOptions {
                    tasks: None,
                    media: Some(self.diagram_media(cx)),
                    row_key: row.id.clone(),
                    veil: None,
                    cache: (!render_cache_disabled()).then(|| self.render_cache.clone()),
                    now: cx.frame_time(), // slopty: the frame's moment
                    copy: Some(self.copy_ui_for(&row.id, cx)),
                    link,
                    workspace_root: workspace_root.clone(),
                    code,
                    run: self.run_ui(cx), // slopty: Run in terminal.
                    code_menu: Some(self.code_menu_ui(cx)),
                    body: self.markdown_body(), // slopty: a card's run reads compact (#24).
                };
                let highlight = self.code_highlight_for(&row.id, &tree, Some(*block_ix), cx);
                render::render_block(
                    &top.block,
                    *block_ix,
                    *block_ix,
                    &opts,
                    &theme,
                    window,
                    highlight
                        .get(block_ix)
                        .and_then(|o| o.as_deref())
                        .map(|document| document.lines.as_slice()),
                )
            }
            RowKind::LiveMarkdown { tree, block_ix } => {
                let link = self.link_ui(cx);
                let tree = self.inline_code_tree(tree, link.as_ref(), cx);
                let tree = self.chat_chips(&row.id, tree, cx); // slopty
                let Some(top) = tree.blocks.get(*block_ix) else {
                    return gpui::Empty.into_any_element();
                };
                let code = self.code_uis_for(&row.id, &top.block, *block_ix, cx);
                // Per-appended-chunk fade veil (opacity only — layout commits
                // instantly). Reduced motion renders with no veil at all.
                // Baseline rows (text already streamed when the transcript
                // attached) start seeded: the existing reply must not fade in
                // on a session switch — only fresh appends animate.
                let reduced_motion = motion::reduced_motion(cx);
                if reduced_motion {
                    // Motion can resume mid-stream (background pause, or the OS
                    // setting flipping back). Text painted meanwhile is already
                    // on screen, so the next veil seeds it rather than
                    // dissolving the reply in again.
                    self.veils.remove(&row.id);
                    self.veil_baseline.insert(row.id.clone());
                }
                let seed_history = !self.veils.contains_key(&row.id)
                    && self.historical_markdown.contains_key(&row.id);
                let veil = (!reduced_motion).then(|| {
                    self.veils
                        .entry(row.id.clone())
                        .or_insert_with(|| {
                            if seed_history || self.veil_baseline.contains(&row.id) {
                                Rc::new(RefCell::new(RowVeil::seeded()))
                            } else {
                                Rc::default()
                            }
                        })
                        .clone()
                });
                // A streaming fence may still be growing, so only blocks the
                // reply has already moved past draw diagrams: a later row of
                // the same entry proves this block is complete. The tail
                // keeps its source until the next block or completion, and
                // per-token commits never start a render.
                let settled = self
                    .rows
                    .get(ix + 1)
                    .is_some_and(|next| next.entry_id == row.entry_id);
                let opts = RenderOptions {
                    tasks: None,
                    media: settled.then(|| self.diagram_media(cx)),
                    row_key: row.id.clone(),
                    veil: veil.clone(),
                    cache: (!render_cache_disabled()).then(|| self.render_cache.clone()),
                    now: motion::pulse_moment(cx.entity_id(), cx), // slopty: the veil's tick, which drew it
                    copy: Some(self.copy_ui_for(&row.id, cx)),
                    link,
                    workspace_root: workspace_root.clone(),
                    code,
                    run: self.run_ui(cx), // slopty: Run in terminal.
                    code_menu: Some(self.code_menu_ui(cx)),
                    body: self.markdown_body(), // slopty: a card's run reads compact (#24).
                };
                if seed_history && let Some(veil) = &veil {
                    let historical = &self.historical_markdown[&row.id];
                    if let RowKind::LiveMarkdown { tree, block_ix } = &historical.kind
                        && let Some(top) = tree.blocks.get(*block_ix)
                    {
                        // Use the renderer's own nested element keys and text
                        // flattening, but never cache/paint the historical tree.
                        let seed_opts = RenderOptions {
                            cache: None,
                            link: None,
                            // Seed fences as source text, never request diagrams.
                            media: None,
                            ..opts.clone()
                        };
                        let _ = render::render_block(
                            &top.block, *block_ix, *block_ix, &seed_opts, &theme, window, None,
                        );
                        veil.borrow_mut().finish_seeding();
                    }
                }
                let highlight = self.code_highlight_for(&row.id, &tree, Some(*block_ix), cx);
                let timer = frame_stats_enabled().then(crate::clock::stopwatch);
                let el = render::render_block(
                    &top.block,
                    *block_ix,
                    *block_ix,
                    &opts,
                    &theme,
                    window,
                    highlight
                        .get(block_ix)
                        .and_then(|o| o.as_deref())
                        .map(|document| document.lines.as_slice()),
                );
                if let Some(start) = timer {
                    record_live_frame_us(crate::clock::spent(start).as_micros() as u64);
                }
                // The attach pass for this row is done (every element rendered
                // above seeded its baseline synchronously): elements appearing
                // from the NEXT pass on are newly streamed and fade normally.
                if let Some(veil) = &veil {
                    veil.borrow_mut().finish_seeding();
                }
                // Share the loaders' bounded clock. A display-frame callback
                // here would pin the transcript to 60/120Hz for the whole
                // stream, bypassing the clock even with no loader mounted.
                if veil.is_some_and(|v| v.borrow().is_fading()) {
                    motion::pulse_lease(cx.entity_id(), cx);
                }
                el
            }
            RowKind::ToolGroup { tools, summary } => {
                self.render_tool_group(&row.id, tools, summary, &theme, cx)
            }
            RowKind::InputChip { label, header, resolved } => {
                input_chip(&row.id, label.clone(), header.clone(), *resolved, &theme)
            }
            RowKind::GeneratedImage {
                owner,
                path,
                name,
                mime_type,
            } => self.render_generated_image(&row.id, owner, path, name, mime_type, cx),
            RowKind::Visual { owner, path, title } => {
                self.render_visual(&row.id, owner, path, title, cx)
            }
            // slopty: a title and one sentence (`extras::render_error_card`).
            RowKind::ErrorChip { message } => self.render_error_card(&row, message, None, &theme, cx),
            RowKind::ForkMarker { source_title, .. } => fork_marker(source_title.clone(), &theme),
            // slopty: Slopty's own rows.
            RowKind::ErrorCard { .. }
            | RowKind::UpdateNeeded { .. }
            | RowKind::Marker { .. }
            | RowKind::PlanHeader { .. }
            | RowKind::PlanFold { .. }
            | RowKind::TurnChanges { .. } => self.render_extra_row(&row, &theme, cx),
            RowKind::TaskNote { task } => self.render_task_note(&row, task, &theme, cx),
            RowKind::Notice { note } => {
                self.render_notice(&row, note, done_trailing.take(), &theme, cx)
            }
            RowKind::Page { owner, card } => self.render_page(&row, owner, card, window, cx),
        };
        // slopty: a plan's rows draw as one card.
        let (inner, top_gap) = self.plan_card_segment(ix, inner, top_gap, &theme);
        // slopty: the transcript's words in the accessibility tree: a markdown block is a
        // paragraph saying its plain text, and a message opens at an article named for who said
        // it ("You" round the person's bubble, the agent's name round the reply's first row).
        let inner = match &row.kind {
            RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => {
                // The words as drawn: a file link by its name, a chat link as its chip.
                let link = self.link_ui(cx);
                let tree = self.inline_code_tree(tree, link.as_ref(), cx);
                let tree = self.chat_chips(&row.id, tree, cx);
                let words = tree.blocks.get(*block_ix).map(|top| block_words(&top.block));
                div()
                    .id(SharedString::from(format!("{}-words", row.id)))
                    .role(gpui::Role::Paragraph)
                    .aria_label(SharedString::from(words.unwrap_or_default()))
                    .child(inner)
                    .into_any_element()
            }
            // slopty: a run's first words are its task, which its parent's agent wrote.
            RowKind::User { .. } if task => message_article(&row.id, "Task", inner),
            RowKind::User { .. } => message_article(&row.id, "You", inner),
            _ => inner,
        };
        // slopty: a reply opens at its first row, whatever it is, so its first calls are heard
        // as its own (#10); what no one said is no one's message (#8).
        let opens_entry = ix == 0 || self.rows[ix - 1].entry_id != row.entry_id;
        let inner = if opens_entry
            && !matches!(row.kind, RowKind::User { .. })
            && !self.unsaid_entries.contains(&row.entry_id)
        {
            message_article(&row.id, agent, inner)
        } else {
            inner
        };
        // slopty: where a jump landed flashes and fades, as Claude's reply flash does (#13).
        let now = cx.frame_time();
        let inner = match self.landed.as_ref().filter(|(id, at, _)| {
            id == &row.id && now.saturating_duration_since(*at) < LANDING_FLASH.total()
        }) {
            Some((_, _, epoch)) => div()
                .relative()
                .child(
                    div()
                        .absolute()
                        .top(px(-4.0))
                        .bottom(px(-4.0))
                        .left(px(-6.0))
                        .right(px(-6.0))
                        .rounded(px(TRANSCRIPT_CARD_RADIUS))
                        .with_animation(
                            SharedString::from(format!("landed-{}-{epoch}", row.id)),
                            LANDING_FLASH.animation(),
                            |flash, t| flash.bg(crate::theme::ink(0.06 * (1.0 - t))),
                        ),
                )
                .child(inner)
                .into_any_element(),
            None => inner,
        };
        // Diagram fences this row just requested start rendering after layout.
        self.ensure_diagram_worker(cx);
        // slopty: and the images it showed are held, read when new.
        let markdown = matches!(row.kind, RowKind::Markdown { .. } | RowKind::LiveMarkdown { .. });
        self.hold_md_images(&row.id, markdown, cx);

        // slopty: the last row's lane holds its working line, then its strip: one lane that
        // never shrinks while the row stays last, so Stop and a turn's end move nothing.
        let lane = if is_last {
            self.last_row_lane(&row.id, trailer.or(strip), cx.entity_id())
        } else {
            strip
        };
        let entry_id = row.entry_id.clone();
        let row_id = row.id.clone();
        let outer = div()
            .id(row.id.clone())
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    let next = Some((row_id.clone(), entry_id.clone()));
                    if this.hovered_entry != next {
                        let entry_changed = this
                            .hovered_entry
                            .as_ref()
                            .is_none_or(|(_, entry)| entry != &entry_id);
                        this.hovered_entry = next;
                        if entry_changed {
                            cx.notify();
                        }
                    }
                } else if this
                    .hovered_entry
                    .as_ref()
                    .is_some_and(|(row, _)| row == &row_id)
                {
                    // Only the row that OWNS the current reveal may clear it —
                    // a stale leave from an earlier row must not blank the
                    // strip the newly entered row just lit.
                    this.hovered_entry = None;
                    cx.notify();
                }
            }))
            .w_full()
            .flex()
            .justify_center()
            .pt(px(top_gap))
            .pb(px(bottom_pad))
            // Keep side gutters as the configurable column shrinks to fit;
            // a docked side card's room comes off the right.
            // slopty: in a card, its rows start under its title.
            .pl(px(if self.fit.is_some() { subagents::CHILD_INSET } else { self.row_inset() }))
            .pr(px(if self.fit.is_some() {
                Theme::SPACE_MD
            } else {
                self.row_inset() + self.right_reserve
            }))
            .child(
                div()
                    .w_full()
                    .max_w(px(self.content_width))
                    .min_w_0()
                    .child(inner)
                    .children(lane)
                    .children(self.footer.clone().filter(|_| is_last).map(|footer| Self::footer_frame(footer, cx))),
            );
        outer.into_any_element()
    }

    /// slopty: the last row's lane: `body` (its working line or its strip) at no less than the
    /// tallest the lane has drawn while this row was last.
    /// A lane that grew asks its view to draw again, so what is drawn always reads the floor
    /// it holds (a retained frame never keeps an older one).
    fn last_row_lane(
        &mut self,
        row_id: &SharedString,
        body: Option<AnyElement>,
        owner: gpui::EntityId,
    ) -> Option<AnyElement> {
        let floor = match &self.last_lane {
            Some((id, floor)) if id == row_id => floor.clone(),
            _ => {
                let floor = Rc::new(Cell::new(ROW_LANE_HEIGHT));
                self.last_lane = Some((row_id.clone(), floor.clone()));
                floor
            }
        };
        let body = body?;
        let probe = floor.clone();
        Some(
            div()
                .relative()
                .w_full()
                .min_h(px(floor.get()))
                .child(body)
                .child(
                    canvas(
                        move |bounds, window, _| {
                            let height = f32::from(bounds.size.height);
                            if height > probe.get() + 0.5 {
                                probe.set(height);
                                window.on_next_frame(move |_, cx| cx.notify(owner));
                            }
                        },
                        |_, _, _, _| (),
                    )
                    .absolute()
                    .inset_0(),
                )
                .into_any_element(),
        )
    }

    /// slopty: the focus of `entry`'s strip Copy, made on first use and kept while its strip is
    /// drawn: the keyboard reaches it, and its focus coming or going draws the strip again (#9).
    fn strip_focus(
        &mut self,
        entry: &SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::FocusHandle {
        self.strip_drawn.insert(entry.clone());
        if let Some(focus) = self.strip_focus.get(entry) {
            return focus.copy.clone();
        }
        let copy = cx.focus_handle().tab_stop(true);
        let redraw = [
            cx.on_focus(&copy, window, |_, _, cx| cx.notify()),
            cx.on_blur(&copy, window, |_, _, cx| cx.notify()),
        ];
        self.strip_focus.insert(entry.clone(), StripFocus { copy: copy.clone(), _redraw: redraw });
        copy
    }

    fn copy_message(&mut self, entry_id: SharedString, text: SharedString, cx: &mut Context<Self>) {
        cx.stop_propagation();
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        // slopty: the house's copy span; its end redraws what read it (a global read).
        motion::copy_feedback(copy_message_key(&entry_id), cx);
        cx.notify();
    }

    /// Copy-button wiring for one row's code blocks ([`render::CopyUi`]):
    /// click writes the block's code to the clipboard and shows a transient
    /// "Copied" check on that block for ~1.2s (overlay — no layout shift).
    fn copy_ui_for(&self, row_id: &SharedString, cx: &mut Context<Self>) -> render::CopyUi {
        let copied_ix = self
            .copied_code
            .as_ref()
            .filter(|(id, _)| id == row_id)
            .map(|(_, ix)| *ix);
        let row_key = row_id.clone();
        let entity = cx.weak_entity();
        let handler: Rc<dyn Fn(usize, SharedString, &mut Window, &mut gpui::App)> =
            Rc::new(move |ix, code, _window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                let row_key = row_key.clone();
                entity
                    .update(cx, |this, cx| {
                        this.copied_code = Some((row_key, ix));
                        this.copied_clear = Some(cx.spawn(async move |this, cx| {
                            // slopty: the house's copy span.
                            cx.background_executor().timer(motion::COPIED_FOR).await;
                            this.update(cx, |this, cx| {
                                this.copied_code = None;
                                this.copied_clear = None;
                                cx.notify();
                            })
                            .ok();
                        }));
                        cx.notify();
                    })
                    .ok();
            });
        render::CopyUi { handler, copied_ix }
    }

    /// Interactive layout/scroll plumbing for one agent Markdown fence. The
    /// persisted Fit choice is global, while each block retains only its own
    /// ephemeral horizontal offset and hover/drag state.
    fn code_ui_for(
        &mut self,
        row_id: &SharedString,
        block_ix: usize,
        cx: &mut Context<Self>,
    ) -> render::CodeUi {
        let key: SharedString = format!("{row_id}#code{block_ix}").into();
        let runtime = self.code_fences.entry(key.clone()).or_default();
        render::code_ui_for(
            key,
            runtime,
            cx.weak_entity(),
            |transcript| &mut transcript.code_fences,
        )
    }
    fn code_uis_for(
        &mut self,
        row_id: &SharedString,
        block: &Block,
        block_ix: usize,
        cx: &mut Context<Self>,
    ) -> Option<HashMap<usize, render::CodeUi>> {
        let indices = render::code_block_indices(block, block_ix);
        (!indices.is_empty()).then(|| {
            indices
                .into_iter()
                .map(|ix| (ix, self.code_ui_for(row_id, ix, cx)))
                .collect()
        })
    }

    /// Request highlights for the code blocks of a tree. `only` limits to one
    /// block index (split rows); `None` covers the whole tree (live rows).
    fn code_highlight_for(
        &mut self,
        row_id: &SharedString,
        tree: &Arc<BlockTree>,
        only: Option<usize>,
        cx: &mut Context<Self>,
    ) -> HashMap<usize, Option<Arc<slopty_syntax::HighlightedDocument>>> {
        let mut out = HashMap::new();
        for (ix, top) in tree.blocks.iter().enumerate() {
            if only.is_some_and(|o| o != ix) {
                continue;
            }
            if let Block::CodeBlock { language, code } = &top.block
                && let Some(lang) = language
                    .as_deref()
                    .and_then(slopty_syntax::language_for_alias)
            {
                out.insert(
                    ix,
                    self.highlights.request(row_id.clone(), ix, lang, code, cx),
                );
            }
        }
        out
    }

    fn tool_diff_highlight_for(
        &mut self,
        row_id: &SharedString,
        tool_ix: usize,
        detail: &ToolDetail,
        cx: &mut Context<Self>,
    ) -> Option<Arc<crate::changes::DiffHighlights>> {
        let ToolDetail::Diff {
            file,
            old_text,
            new_text,
        } = detail
        else {
            return None;
        };
        let cache_row: SharedString = format!("{row_id}#tool-diff-{tool_ix}").into();
        let old = match old_text {
            Some(source) => {
                let path = file.old_path.as_deref().unwrap_or(&file.path);
                let lang = slopty_syntax::language_for_path(path)?;
                Some(
                    self.highlights
                        .request(cache_row.clone(), 0, lang, source, cx)?,
                )
            }
            None => None,
        };
        let new = match new_text {
            Some(source) => {
                let lang = slopty_syntax::language_for_path(&file.path)?;
                Some(self.highlights.request(cache_row, 1, lang, source, cx)?)
            }
            None => None,
        };
        Some(Arc::new(crate::changes::DiffHighlights { old, new }))
    }

    fn render_tool_group(
        &mut self,
        row_id: &SharedString,
        tools: &Arc<Vec<ToolItem>>,
        summary: &Sentence,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut fold = self.folds.get(row_id).copied().unwrap_or_default();
        let frame_now = cx.frame_time(); // slopty: the frame's moment, for its tweens
        let file_opener = self.tool_file_opener(cx);
        // slopty: what calls say beyond zeron's chip; a running timer draws again each second.
        let chip_facts = self.chip_facts();
        if tools.iter().any(|tool| {
            !tool.resolved && tool.facts.as_ref().is_some_and(|facts| facts.timer.is_some())
        }) {
            self.tick_soon(cx);
        }
        let (collapses, solo) = group_shape(tools);
        // slopty: a group opens only as the person asks; nothing opens it by itself, not a
        // running call and not one arriving (`inspector.md`, "One fold depth").
        let open = solo || !collapses || fold.open.unwrap_or(false);
        if collapses {
            let reveal = self.tool_group_reveals.entry(row_id.clone()).or_default();
            if reveal
                .rendered_open
                .is_some_and(|previous| previous != open)
            {
                fold.from = reveal.rendered_height;
                fold.toggled_at = Some(crate::clock::now(cx));
                fold.disclosure_at = fold.toggled_at;
                self.folds.insert(row_id.clone(), fold);
            }
            reveal.rendered_open = Some(open);
        }
        // The title shimmer reads "working" while any call or thought of the group still runs.
        // Compute this before the collapsed-body skip, which may empty `tools`.
        let active = collapses && tools.iter().any(|t| !t.resolved);

        // A settled collapsed group has no visible body. Do not construct or
        // format thousands of hidden chips merely to clip them to zero height.
        // Keep the body during closing so the existing fold animation survives.
        let body_visible = open || tool_fold(open, fold.toggled_at, frame_now, cx.reduce_motion()).moving;
        let tools = if body_visible {
            tools.as_slice()
        } else {
            self.tool_images
                .release_row(row_id, Self::tool_images_of, cx);
            &[]
        };
        // Chips render their EFFECTIVE detail: the precomputed doc-resident
        // one, upgraded in place by a fetched sidecar blob (chat2-sync A3).
        // Resolved per paint (a HashMap probe per chip) so fetched content
        // needs no row rebuild — arrival is a cx.notify, like a fold toggle.
        let details: Vec<Option<Arc<ToolDetail>>> = tools
            .iter()
            .map(|tool| {
                // Spawn chips never expand — the subagent doc is the record
                // of what the tool did, and an inline body would only repeat
                // it. The whole chip is the "open that doc" click instead.
                if is_spawn_link(tool) || quiet_read(tool) {
                    return None;
                }
                // Among fetched blobs, the most recently REQUESTED one wins —
                // a tool can carry both a diff and an output ref, and the
                // user's last click decides which upgrade is showing.
                let mut best: Option<(u64, Arc<ToolDetail>)> = None;
                for blob_ref in [&tool.diff_ref, &tool.output_ref].into_iter().flatten() {
                    if let Some(BlobFetch::Ready(detail)) = self.blob_details.get(blob_ref) {
                        let order = self.blob_fetch_order.get(blob_ref).copied().unwrap_or(0);
                        if best.as_ref().is_none_or(|(o, _)| order > *o) {
                            best = Some((order, detail.clone()));
                        }
                    }
                }
                best.map(|(_, d)| d).or_else(|| tool.detail.clone())
            })
            .collect();
        // Full-invocation blocks — with them, EVERY chip expands: the click
        // always answers "what exactly was this call?", output or not.
        let invocations: Vec<Option<Arc<ToolDetail>>> = tools
            .iter()
            .map(|tool| tool.invocation.clone().filter(|_| !is_spawn_link(tool) && !quiet_read(tool)))
            .collect();
        // Fetch affordance under each open detail whose full payload is still
        // sidecar-only: `(ref, label)`. Diff offered first (the richer
        // upgrade), then the output — a fetched ref hands the affordance to
        // the NEXT unfetched one instead of retiring it (both must stay
        // reachable when a tool has both).
        let affordances: Vec<Option<ChipAffordance>> = tools
            .iter()
            .map(|tool| {
                // The currently-displayed ref (same recency rule as
                // `details` above): its affordance is spent; any OTHER
                // Ready ref stays offered as a no-fetch toggle.
                let shown: Option<&SharedString> = {
                    let mut best: Option<(u64, &SharedString)> = None;
                    for blob_ref in [&tool.diff_ref, &tool.output_ref].into_iter().flatten() {
                        if matches!(self.blob_details.get(blob_ref), Some(BlobFetch::Ready(_))) {
                            let order = self.blob_fetch_order.get(blob_ref).copied().unwrap_or(0);
                            if best.is_none_or(|(o, _)| order > o) {
                                best = Some((order, blob_ref));
                            }
                        }
                    }
                    best.map(|(_, r)| r)
                };
                let candidates = [
                    (tool.diff_ref.as_ref(), "diff", None),
                    (tool.output_ref.as_ref(), "output", tool.output_bytes),
                ];
                for (blob_ref, what, bytes) in candidates {
                    let Some(blob_ref) = blob_ref else { continue };
                    let retry = matches!(self.blob_details.get(blob_ref), Some(BlobFetch::Failed));
                    let label = match self.blob_details.get(blob_ref) {
                        Some(BlobFetch::Ready(_)) => {
                            if shown == Some(blob_ref) {
                                continue;
                            }
                            format!("Show full {what}")
                        }
                        Some(BlobFetch::Loading(_)) => format!("Loading full {what}…"),
                        // slopty: what happened, then one action (#18).
                        Some(BlobFetch::Failed) => format!("Couldn't load the full {what}."),
                        None => match bytes {
                            // slopty: one size formatter ("12.3 KB").
                            Some(b) => format!("Show full {what} ({})", slopty_view_doc::bytes_short(b)),
                            None => format!("Show full {what}"),
                        },
                    };
                    return Some(ChipAffordance {
                        blob_ref: blob_ref.clone(),
                        label: SharedString::from(label),
                        retry,
                    });
                }
                None
            })
            .collect();
        // Which chips have their detail block open (render-local, analytic —
        // the FINAL state; a mid-tween detail already counts as its target).
        let detail_folds: Vec<FoldState> = details
            .iter()
            .zip(&invocations)
            .enumerate()
            .map(|(ix, (detail, invocation))| {
                if detail.is_none() && invocation.is_none() && !chip_has_images(&tools[ix]) {
                    return FoldState::default();
                }
                self.tool_details
                    .get(&SharedString::from(format!("{row_id}#d{ix}")))
                    .copied()
                    .unwrap_or_default()
            })
            .collect();
        let detail_opens: Vec<bool> = details
            .iter()
            .zip(&invocations)
            .zip(&detail_folds)
            .zip(tools.iter())
            .map(|(((detail, invocation), fold), tool)| {
                // slopty: a step's detail opens only as the person asks, a live thought's too.
                (detail.is_some() || invocation.is_some() || chip_has_images(tool))
                    && fold.open.unwrap_or(false)
            })
            .collect();
        // Image previews exist only while their chip's body is mounted (open,
        // or tweening shut). Every other chip releases its pixels.
        let chip_images = self.hold_tool_images(row_id, tools, &detail_opens, &detail_folds, cx);
        let detail_highlights: Vec<Option<Arc<crate::changes::DiffHighlights>>> = details
            .iter()
            .enumerate()
            .map(|(ix, detail)| {
                detail
                    .as_deref()
                    .filter(|_| detail_opens[ix])
                    .and_then(|detail| self.tool_diff_highlight_for(row_id, ix, detail, cx))
            })
            .collect();
        let base_row_height = if collapses {
            TOOL_TREE_ROW_HEIGHT
        } else {
            CHIP_HEIGHT
        };
        let mut motion_active = false;
        let row_heights: Vec<f32> = details
            .iter()
            .zip(&invocations)
            .zip(&affordances)
            .zip(&detail_opens)
            .zip(&detail_folds)
            .zip(tools.iter())
            .map(
                |(((((detail, invocation), affordance), open), fold), tool)| {
                    let target = if *open {
                        base_row_height
                            + invocation.as_deref().map_or(0.0, detail_height)
                            + if chip_has_images(tool) {
                                TOOL_IMAGE_STRIP_HEIGHT
                            } else {
                                0.0
                            }
                            + detail.as_deref().map_or(0.0, detail_height)
                            + if affordance.is_some() {
                                BLOB_AFFORDANCE_HEIGHT
                            } else {
                                0.0
                            }
                    } else {
                        base_row_height
                    };
                    if !cx.reduce_motion() {
                        if let Some(at) = fold.toggled_at {
                            let fold_now = tool_fold(*open, Some(at), frame_now, false);
                            if fold_now.moving {
                                motion_active = true;
                            }
                            return motion::lerp(
                                fold.from + base_row_height - CHIP_CARD_HEIGHT,
                                target,
                                fold_now.share,
                            );
                        }
                    }
                    target
                },
            )
            .collect();
        let reduce_motion = cx.reduce_motion();
        let now = cx.frame_time(); // slopty: the frame's moment
        let reveal_progress: Vec<f32> = (0..tools.len())
            .map(|ix| {
                let start = self
                    .tool_group_reveals
                    .get(row_id)
                    .and_then(|reveal| reveal.starts.get(ix))
                    .copied()
                    .flatten();
                tool_row_reveal_progress(start, now, reduce_motion)
            })
            .collect();
        let connector_progress: Vec<f32> = (0..tools.len())
            .map(|ix| {
                let start = self
                    .tool_group_reveals
                    .get(row_id)
                    .and_then(|reveal| reveal.starts.get(ix))
                    .copied()
                    .flatten();
                tool_connector_reveal_progress(start, now, reduce_motion)
            })
            .collect();
        let header_reveal = tool_row_reveal_progress(
            self.tool_group_reveals
                .get(row_id)
                .and_then(|reveal| reveal.header_started_at),
            now,
            reduce_motion,
        );
        if header_reveal < 1.0
            || reveal_progress.iter().any(|progress| *progress < 1.0)
            || connector_progress.iter().any(|progress| *progress < 1.0)
        {
            motion_active = true;
        }
        let revealed_height = CHIPS_TOP_PAD
            + row_heights
                .iter()
                .zip(&reveal_progress)
                .map(|(height, progress)| height * progress)
                .sum::<f32>();
        let viewport_height = revealed_height;
        let target = if open { viewport_height } else { 0.0 };
        // slopty: a running call's row sweeps in phase with its group's title (#14).
        let running_call = open && tools.iter().any(sweeps);
        let shimmer_phase = if (active || running_call) && !reduce_motion {
            motion::pulse_lease(cx.entity_id(), cx);
            // slopty: the pulse's tick, which drew it, on the shimmer's one clock.
            Some(shimmer_ms(motion::pulse_moment(cx.entity_id(), cx)))
        } else {
            None
        };
        let row_shimmer = shimmer_phase.filter(|_| running_call);
        let shimmer_phase = shimmer_phase.filter(|_| active);
        let disclosure_progress = if reduce_motion {
            if open { 1.0 } else { 0.0 }
        } else {
            tool_disclosure_progress(open, fold, now)
        };

        // The summary rolls as the group grows ("Thought 2 times" → "Thought
        // 3 times · Ran 1 command"), like the working indicator's word.
        let rolled_summary = crate::roll_text::rolling(
            format!("tool-group-title-{row_id}"),
            summary.text.clone(),
            reduce_motion,
            crate::clock::now(cx),
        );
        let toggle_id = row_id.clone();
        // A quiet summary sits above the activity rail; its chevron occupies
        // the same gutter as the rounded task-tree elbows below it.
        // slopty: a button the keyboard reaches, Enter or Space opening it (#9).
        // slopty: its chevron arrives under the pointer and stays while the group is open (Amp);
        // on a touch screen it is there at rest.
        let header_hover = format!("{row_id}-hdr-hover");
        let chevron_shown = motion::revealed_by_hover(&header_hover).max(disclosure_progress);
        let header = crate::surface_chrome::key_stop(div().id(SharedString::from(format!("{row_id}-hdr"))), theme)
            .hover_fade(header_hover)
            .rounded(px(4.0))
            // slopty: the group reads as a button named by its summary, open or not.
            .role(gpui::Role::Button)
            .aria_label(summary.text.clone())
            .aria_expanded(open)
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pr(px(4.0))
            // slopty: a finger's row on a touch screen (#23).
            .h(px(tool_group_header_height()))
            .cursor_pointer()
            .text_size(px(TOOL_LABEL_SIZE))
            .line_height(px(TOOL_LABEL_LINE_HEIGHT))
            // Quiet even when children failed: agents routinely have failed
            // probes mid-work, and a red HEADER read as "this whole step
            // broke" (user report). Failures still show on the individual
            // chips (destructive tint, slopty tool-chip.tsx) and in the
            // clause's "(N failed)".
            .text_color(theme.text_muted)
            .hover(|s| s.text_color(theme.text))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_fold(toggle_id.clone(), viewport_height, cx);
                cx.notify();
            }))
            .child(
                div()
                    // Keep the title adjacent to its disclosure affordance.
                    .w(px(22.0))
                    .h(px(18.0))
                    .flex_none()
                    .relative()
                    .child(
                        crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                            .absolute()
                            .left(px(ACTIVITY_TRUNK_X - 7.0))
                            .top(px(2.0))
                            .size(px(14.0))
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                -std::f32::consts::FRAC_PI_2 * (1.0 - disclosure_progress),
                            )))
                            .text_color(theme.text_muted)
                            .opacity(chevron_shown),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .h(px(TOOL_LABEL_LINE_HEIGHT))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(rolled_summary.unwrap_or_else(|| {
                        tool_group_title(summary.text.clone(), &summary.marks, shimmer_phase, theme)
                    })),
            );

        let chips = div()
            .pt(px(CHIPS_TOP_PAD))
            .flex()
            .flex_col()
            .gap(px(CHIP_GAP))
            .children(tools.iter().enumerate().map(|(ix, tool)| {
                let reveal = reveal_progress[ix];
                let connector_reveal = connector_progress[ix];
                let content_reveal = tool_connector_parts(connector_reveal, ix > 0).1;
                let continuation_reveal =
                    tool_connector_continuation(connector_progress.get(ix + 1).copied());
                let row_height = row_heights[ix];
                // slopty: a spawn is its subagent's card, which opens in place and drills in
                // (`transcript/subagents.rs`), only as the person asks.
                if is_spawn_link(tool) {
                    return self.render_subagent_card(row_id, ix, tool, false, collapses, theme, cx);
                }
                let detail = details[ix].clone();
                let invocation = invocations[ix].clone();
                if detail.is_none() && invocation.is_none() && !chip_has_images(tool) {
                    return reveal_tool_row(
                        tool_chip(
                            SharedString::from(format!("{row_id}#t{ix}")),
                            tool,
                            collapses,
                            solo,
                            ix > 0,
                            ix + 1 < tools.len(),
                            content_reveal,
                            connector_reveal,
                            continuation_reveal,
                            file_opener.as_ref(),
                            Some(&chip_facts),
                            row_shimmer,
                            theme,
                            cx.entity_id(),
                            cx,
                        ),
                        row_height,
                        reveal,
                    );
                }
                let affordance = affordances[ix].clone();
                let open = detail_opens[ix];
                let dfold = detail_folds[ix];
                let key = SharedString::from(format!("{row_id}#d{ix}"));
                // Ordinary tools expand into muted text along the same column.
                // Subagent fallbacks retain their card; explicit heights keep
                // the row and group fold animations in sync.
                let animating = dfold.epoch > 0
                    && dfold
                        .toggled_at
                        .is_some_and(|at| frame_now.saturating_duration_since(at) < FOLD_TWEEN_WINDOW);
                let toggle_key = key.clone();
                let mut card = div()
                    .my(px((base_row_height - CHIP_CARD_HEIGHT) / 2.0))
                    .when(collapses, |el| el.ml(px(activity_text_gap(solo))))
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .when(!collapses, |card| {
                        card.rounded(px(TRANSCRIPT_CARD_RADIUS)) // slopty: the column's card (#19).
                            .border_1()
                            .border_color(theme.border)
                            .bg(crate::theme::ink(0.03))
                    })
                    .child(
                        // slopty: a button named by its call, which the keyboard reaches and
                        // Enter or Space opens (#9, #10).
                        chip_a11y(
                            crate::surface_chrome::key_stop_inset(div().id(key.clone()), theme),
                            tool,
                            Some(open),
                        )
                            .hover_fade(format!("{key}-hover"))
                            .rounded(px(4.0))
                            .h(px(if collapses {
                                CHIP_CARD_HEIGHT
                            } else {
                                CHIP_HEADER_HEIGHT
                            }))
                            .flex_none()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                let entry =
                                    this.tool_details.entry(toggle_key.clone()).or_default();
                                let currently_open = entry.open.unwrap_or(open);
                                entry.from = row_height - base_row_height + CHIP_CARD_HEIGHT;
                                entry.open = Some(!currently_open);
                                entry.epoch += 1;
                                entry.toggled_at = Some(crate::clock::now(cx));
                                cx.notify();
                            }))
                            .child(chip_header(
                                tool,
                                open,
                                motion::revealed_by_hover(&format!("{key}-hover")),
                                file_opener.as_ref(),
                                Some(&chip_facts),
                                row_shimmer,
                                theme,
                                cx.entity_id(),
                                cx,
                            )),
                    );
                // The body stays mounted while the close tween shrinks over it.
                // Invocation first (what was asked), then output/diff (what
                // came back), separated by a small gap.
                if open || animating {
                    // slopty: the details fade in after the row opens, and out before it shuts.
                    let contents = tool_fold(open, dfold.toggled_at.filter(|_| dfold.epoch > 0), frame_now, cx.reduce_motion());
                    let mut panel = div()
                        .flex_none()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .when(contents.opacity < 1.0, |panel| panel.opacity(contents.opacity))
                        .when(contents.rise > 0.0, |panel| panel.mt(px(contents.rise)));
                    if let Some(invocation) = invocation.as_deref() {
                        panel = panel
                            .child(
                                div()
                                    .h(px(DETAIL_SEPARATOR))
                                    .flex_none()
                                    .when(!collapses, |line| line.bg(crate::theme::hairline(0.06))),
                            )
                            .child(if matches!(tool.call, ToolCall::Exec { .. }) {
                                // slopty: a command reads highlighted, as the shell would show it.
                                extras::command_body(invocation, &format!("{key}-call"), theme)
                                    .unwrap_or_else(|| {
                                        detail_body(invocation, None, &format!("{key}-call"), theme)
                                    })
                            } else {
                                detail_body(invocation, None, &format!("{key}-call"), theme)
                            });
                    }
                    if let Some(images) = chip_images[ix].clone() {
                        panel = panel.child(
                            self.render_tool_image_strip(&key, images, collapses, theme, cx),
                        );
                    }
                    if let Some(detail) = detail.as_deref() {
                        panel = panel
                            .child(
                                div()
                                    .h(px(DETAIL_SEPARATOR))
                                    .flex_none()
                                    .when(!collapses, |line| line.bg(crate::theme::hairline(0.06))),
                            )
                            .child(detail_body(
                                detail,
                                detail_highlights[ix].clone(),
                                &format!("{key}-out"),
                                theme,
                            ));
                    }
                    if let Some(ChipAffordance { blob_ref, label, retry }) = affordance {
                        let loading = matches!(
                            self.blob_details.get(&blob_ref),
                            Some(BlobFetch::Loading(_))
                        );
                        let mut row = div()
                            .id(SharedString::from(format!("{key}-blob")))
                            .h(px(BLOB_AFFORDANCE_HEIGHT))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(TOOL_TEXT_SIZE))
                            .text_color(theme.text_faint)
                            .child(label.clone())
                            .when(retry, |row| {
                                row.child(div().text_color(theme.text_muted).child("Retry"))
                            });
                        if !loading {
                            // slopty: a button the keyboard reaches (#9).
                            row = crate::surface_chrome::key_stop(row, theme)
                                .role(gpui::Role::Button)
                                // slopty: its Retry says what it fetches again (#18).
                                .aria_label(if retry {
                                    SharedString::from(
                                        label.replace("Couldn't load", "Retry loading").trim_end_matches('.').to_owned(),
                                    )
                                } else {
                                    label
                                })
                                .rounded(px(4.0))
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.text_muted))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.spawn_blob_fetch(blob_ref.clone(), cx);
                                    cx.notify();
                                }));
                        }
                        panel = panel.child(row);
                    }
                    card = card.child(panel);
                }
                let card = card.h(px(row_height - base_row_height + CHIP_CARD_HEIGHT));
                let card = div().min_w_0().flex_1().child(card);
                let row = div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_row()
                    // Stretch the line alongside the expanded text.
                    .when(collapses, |row| {
                        row.child(if solo {
                            solo_rail(tool, base_row_height, theme)
                        } else {
                            activity_rail(
                                tool,
                                ix > 0,
                                ix + 1 < tools.len(),
                                connector_reveal,
                                continuation_reveal,
                                base_row_height,
                                theme,
                            )
                        })
                    })
                    .child(card.when(collapses && content_reveal < 1.0, |card| {
                        card.relative()
                            .top(px(4.0 * (1.0 - content_reveal)))
                            .opacity(content_reveal)
                    }))
                    .into_any_element();
                reveal_tool_row(row, row_height, reveal)
            }));

        let chips = chips.into_any_element();

        // Evaluate the group on the same clock as its disclosure. This also
        // gives completion a gentle close after the last arrival finishes,
        // without restarting an element animation when the list remounts it.
        let group_fold = tool_fold(open, fold.toggled_at, now, reduce_motion);
        if group_fold.moving {
            motion_active = true;
        }
        let body_height = if fold.toggled_at.is_some() && !reduce_motion {
            motion::lerp(fold.from, target, group_fold.share)
        } else {
            target
        };
        if let Some(reveal) = self.tool_group_reveals.get_mut(row_id) {
            reveal.rendered_height = body_height;
        }
        let body: AnyElement = if !collapses {
            chips.into_any_element()
        } else {
            div()
                .overflow_hidden()
                .h(px(body_height))
                // slopty: the calls fade in after the height opens, and out before it lands.
                .child(
                    div()
                        .when(group_fold.opacity < 1.0, |chips| chips.opacity(group_fold.opacity))
                        .when(group_fold.rise > 0.0, |chips| chips.mt(px(group_fold.rise)))
                        .child(chips),
                )
                .into_any_element()
        };

        let view = cx.entity_id();
        div()
            .relative()
            .flex()
            .flex_col()
            // Tool summaries and cards are code-adjacent chrome. Detail bodies
            // retain their explicit mono/diff typography below this boundary.
            .font_family(theme.font_sans_fixed.clone())
            .when(collapses && !solo, |el| {
                el.child(reveal_tool_row(
                    header.into_any_element(),
                    tool_group_header_height(),
                    header_reveal,
                ))
            })
            .child(body)
            .when(motion_active, |group| {
                group.child(
                    canvas(
                        |_, _, _| (),
                        move |_, _, window, _| {
                            window.on_next_frame(move |_, cx| cx.notify(view));
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            })
            .into_any_element()
    }
}

/// A sent message's text with its file-mention chips. The same recipe as the
/// markdown renderer's inline code (`flat_text_element`): chip ranges shape in
/// the mono font at the spectrum's `code_text`, [`StyledText`] supplies wrapped glyph
/// geometry through its layout handle, and a canvas paints the rounded
/// `code_wash` *beneath* the glyphs — so chips wrap, clip, and scroll exactly
/// like the text they decorate.
///
/// Per-frame cost while an assistant message streams below: shaping hits
/// gpui's line-layout cache (identical text + runs ⇒ reuse) and the underlay
/// repaints O(chips) quads — no layout work, no re-projection (spans were
/// computed once in [`rows_for_entry`]).
/// The user bubble's text: runs split at mention-chip boundaries (one plain
/// run when there are none), with the same selection machinery as rendered
/// markdown — the element registers into the frame's document-ordered
/// registry, so drags select, span into adjacent rows, and Cmd+C copies.
/// Keep routing instructions in the stored prompt for agents, but show a
/// concise attribution in the human transcript (including existing messages).
fn agent_message_display(text: &str) -> String {
    let Some(rest) = text.strip_prefix("[Message from Slopty chat ") else {
        return text.to_owned();
    };
    let Some((header, body)) = rest.split_once("]\n\n") else {
        return text.to_owned();
    };
    let Some((label, id)) =
        header.rsplit_once(". Reply to it with the Slopty `send_message` tool, chat ")
    else {
        return text.to_owned();
    };
    let Some(id) = id.strip_suffix('.') else {
        return text.to_owned();
    };
    let suffix = format!(" ({id})");
    let name = label.strip_suffix(&suffix).unwrap_or(label);
    format!("Message from {name}\n\n{body}")
}

/// The bubble text's runs: body text in the sans font, and an agent attribution's name in bold.
fn user_bubble_runs(text: &str, theme: &Theme) -> Vec<TextRun> {
    // Size and line height flow from the bubble's div like every text child.
    let mut runs = vec![TextRun {
        len: text.len(),
        font: gpui::font(theme.font_sans.clone()),
        color: theme.text,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    // Attribution names are bold sans text, never Markdown/italic. Split
    // the run so selection offsets stay intact.
    if let Some(rest) = text.strip_prefix("Message from ")
        && let Some((name, _)) = rest.split_once("\n\n")
    {
        let bold = "Message from ".len().."Message from ".len() + name.len();
        let mut offset = 0;
        runs = runs
            .into_iter()
            .flat_map(|run| {
                let end = offset + run.len;
                let mut pieces = Vec::new();
                while offset < end {
                    let in_name = bold.contains(&offset);
                    let next = if offset < bold.start {
                        end.min(bold.start)
                    } else if in_name {
                        end.min(bold.end)
                    } else {
                        end
                    };
                    let mut piece = run.clone();
                    piece.len = next - offset;
                    if in_name {
                        piece.font.weight = gpui::FontWeight::BOLD;
                    }
                    pieces.push(piece);
                    offset = next;
                }
                pieces
            })
            .collect();
    }
    runs
}

/// A prompt's Markdown, and for one too long to fold whole, the leading part
/// that fills the fold.
#[derive(Clone)]
pub struct UserMarkdown {
    pub full: Arc<BlockTree>,
    pub folded: Option<Arc<BlockTree>>,
}

/// The leading part of `tree` that fills `lines` lines: whole blocks, then
/// the first items of a list, rows of a table or lines of a fence. Counts
/// source lines, which never outnumber the lines they wrap to, so the part
/// is never shorter than the fold. `None` when the whole tree is that short.
fn folded_markdown(tree: &BlockTree, lines: usize) -> Option<BlockTree> {
    fn take(block: &Block, budget: &mut usize) -> Option<Block> {
        if *budget == 0 {
            return None;
        }
        let spend =
            |budget: &mut usize, lines: usize| *budget = budget.saturating_sub(lines.max(1));
        let runs_lines = |runs: &[InlineRun]| {
            1 + runs
                .iter()
                .map(|r| r.text.matches('\n').count())
                .sum::<usize>()
        };
        Some(match block {
            Block::Paragraph { runs } | Block::Heading { runs, .. } => {
                spend(budget, runs_lines(runs));
                block.clone()
            }
            Block::CodeBlock { language, code } => {
                // The fence's header takes a line of its own.
                spend(budget, 1);
                let kept: Vec<&str> = code.lines().take((*budget).max(1)).collect();
                spend(budget, kept.len());
                Block::CodeBlock {
                    language: language.clone(),
                    code: kept.join("\n"),
                }
            }
            Block::Table {
                header,
                rows,
                align,
            } => {
                spend(budget, 1);
                let kept = rows.len().min((*budget).max(1));
                spend(budget, kept);
                Block::Table {
                    header: header.clone(),
                    rows: rows[..kept].to_vec(),
                    align: align.clone(),
                }
            }
            Block::BlockQuote { children } => Block::BlockQuote {
                children: children
                    .iter()
                    .map_while(|child| take(child, budget))
                    .collect(),
            },
            Block::List {
                ordered_start,
                items,
            } => Block::List {
                ordered_start: *ordered_start,
                items: items
                    .iter()
                    .map_while(|item| {
                        let kept: Vec<Block> =
                            item.iter().map_while(|child| take(child, budget)).collect();
                        (!kept.is_empty()).then_some(kept)
                    })
                    .collect(),
            },
            Block::Math { .. } => {
                spend(budget, 2);
                block.clone()
            }
            Block::Rule => {
                spend(budget, 1);
                block.clone()
            }
        })
    }
    let mut budget = lines;
    let blocks: Vec<Arc<crate::markdown::parser::TopBlock>> = tree
        .blocks
        .iter()
        .map_while(|top| {
            let block = take(&top.block, &mut budget)?;
            Some(Arc::new(crate::markdown::parser::TopBlock {
                range: top.range.clone(),
                block,
            }))
        })
        .collect();
    // Something left out: the last block cut short, or blocks after it.
    let whole = blocks.len() == tree.blocks.len()
        && blocks
            .iter()
            .zip(&tree.blocks)
            .all(|(part, top)| part.block == top.block);
    (!whole).then_some(BlockTree { blocks })
}

/// The render key of a user row's Markdown: its selection, element ids and
/// cached flattening, apart from the row's own.
fn user_markdown_key(row_id: &str) -> SharedString {
    format!("{row_id}:u").into()
}

/// Whether a prompt's Markdown is only unstyled paragraphs: it reads the same
/// as typed, so the bubble keeps its plain text.
fn plain_markdown(tree: &BlockTree) -> bool {
    tree.blocks.iter().all(|top| match &top.block {
        Block::Paragraph { runs } => runs.iter().all(|run| run.style == InlineStyle::default()),
        _ => false,
    })
}

fn user_bubble_text(
    row_id: &SharedString,
    text: SharedString,
    theme: &Theme,
    measured_h: Rc<Cell<f32>>,
    entity_id: gpui::EntityId,
) -> AnyElement {
    let runs = user_bubble_runs(&text, theme);
    let styled = StyledText::new(text.clone()).with_runs(runs);
    let layout = styled.layout().clone();
    let sel_key: std::sync::Arc<str> = format!("{row_id}:u").into();
    let sel_theme = theme.clone();
    let underlay = canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_, hitbox, window, cx| {
            render::paint_text_selection(window, hitbox, &sel_key, &text, &layout, &sel_theme);
            // Passive geometry cache only: no entity update and no notify.
            // `bounds().height` can be the collapsed clip height, so derive
            // the full text height from the wrapped line layouts instead. The
            // click handler reads this exact value as the RESIZE endpoint,
            // while idle layout never feeds back into the transcript.
            let line_count: usize = layout
                .line_layouts()
                .iter()
                .map(|line| line.wrap_boundaries.len() + 1)
                .sum();
            let next_h = (line_count.max(1) as f32) * f32::from(layout.line_height());
            if (measured_h.get() - next_h).abs() > 0.5 {
                measured_h.set(next_h);
                // The first layout is the source of truth for soft wrapping.
                // Invalidate the transcript once so the expander and clip are
                // present even when glyph widths make a short-looking string
                // exceed five visual lines.
                // slopty: told once this frame is drawn: a view notified as its window paints
                // asks for no frame (gpui-fast, as upstream), so the fold this height decides
                // would wait for whatever drew next, and a window at rest kept the bubble as it
                // was folded at the old width (the iPad fuzz's seed 4255).
                cx.defer(move |cx| cx.notify(entity_id));
            }
        },
    )
    .absolute()
    .size_full();
    // Same wrapper as the assistant markdown: user-bubble text is
    // selectable (paint_text_selection above), so it gets the I-beam too.
    render::selectable_text_wrap()
        .child(underlay)
        .child(styled)
        .into_any_element()
}

/// slopty: a hover strip's control: a tab stop whose hairline takes the accent from the
/// keyboard, so focus never changes its size; the ring stands around what it draws, `reach`
/// inside its press ([`strip_press`]).
pub(super) fn strip_control<E: InteractiveElement + Styled>(el: E, theme: &Theme, reach: f32) -> E {
    crate::surface_chrome::focus_ring_within(el.tab_index(0), theme, reach)
}

/// slopty: what a hover strip's control draws, its wash and its icon.
const STRIP_CONTROL_SIZE: f32 = Theme::SPACE_MD * 2.0;

/// slopty: a hover strip control's press: a finger's 44 pt on a touch screen around the
/// [`strip_drawn`] it holds, the layout unchanged (#23); the drawn size under a pointer.
pub(super) fn strip_press(id: impl Into<SharedString>) -> (gpui::Stateful<gpui::Div>, f32) {
    crate::composer::finger_press(id.into(), STRIP_CONTROL_SIZE, STRIP_CONTROL_SIZE)
}

/// slopty: a hover strip control as drawn, for its wash and its icon.
pub(super) fn strip_drawn() -> gpui::Div {
    div()
        .size(px(STRIP_CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(Theme::CONTROL_RADIUS))
}

/// A sent message's non-image attachment: the file's icon and name on the
/// same soft pill the transcript uses for file badges. Its bytes are never
/// read back to the transcript, so there is no thumbnail to wait for.
fn user_file_pill(name: &str, theme: &Theme) -> AnyElement {
    div()
        .h(px(28.0))
        .max_w(px(260.0))
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.0))
        .rounded(px(7.0))
        .bg(theme.ink(0.06))
        .pl(px(2.0))
        .pr(px(10.0))
        .text_size(px(13.0))
        .text_color(theme.text.opacity(0.85))
        .child(
            div()
                .size(px(24.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .bg(crate::file_icons::well_bg(theme))
                .child(
                    crate::file_icons::icon(
                        crate::file_icons::FileIconIdentity::file(name),
                        theme.appearance,
                    )
                    .size(px(16.0)),
                ),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(SharedString::from(name.to_owned())),
        )
        .into_any_element()
}

/// A quiet fork seam. The source gets its own constrained line so long
/// titles cannot widen a narrow side-chat pane. No message metadata lane.
fn fork_marker(source_title: SharedString, theme: &Theme) -> AnyElement {
    // slopty: the hairline the other markers draw (#19).
    let rule = || div().flex_1().min_w_0().h(px(1.0)).bg(theme.border);
    div()
        .py(px(14.0))
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(rule())
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted.opacity(0.7))
                        .child("Forked from"),
                )
                .child(rule()),
        )
        // Centered by flex, not `text_center()`: gpui registers a text line's
        // layer at the line origin with the text's own width, so glyphs shifted
        // by alignment paint outside it and can draw over popovers above them.
        .child(
            div().w_full().min_w_0().flex().justify_center().child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(source_title),
            ),
        )
        .into_any_element()
}

/// A passive one-line chip marking a question the agent asked — the
/// interactive controls live in the composer (chat-view.tsx `InputChip`):
/// 34px row, `rounded-[10px] border-white/[0.08] bg-white/[0.045] px-2
/// text-[12px]`, a 20px `bg-white/[0.09]` icon tile, the medium label, then the
/// truncating value. Neutral tones throughout; resolution never recolors the chip.
/// slopty: the tile's icon says the ask's kind, an open ask's label says what it waits for in
/// the sidebar's words, and assistive tech reads it as a status (#8); its corner and hairline
/// are the column's cards' (#19).
fn input_chip(
    id: &SharedString,
    label: SharedString,
    header: SharedString,
    resolved: bool,
    theme: &Theme,
) -> AnyElement {
    // slopty: one rule with the card's own marks: nothing an ask shows is locked; an approval
    // asks for the person's leave (a key), a confirmation informs (#19).
    let icon = match label.as_ref() {
        "Approval" => crate::icons::KEY_MINIMALISTIC,
        "Plan" => crate::icons::CHECKLIST,
        "Form" => crate::icons::PEN,
        "Confirm" => crate::icons::INFO_CIRCLE,
        _ => crate::icons::CHAT_ROUND_LINE,
    };
    let label: SharedString = match (resolved, label.as_ref()) {
        (true, _) => label,
        (false, "Approval") => "Needs your approval".into(),
        (false, "Plan") => "Plan ready for review".into(),
        (false, _) => "Waiting for your answer".into(),
    };
    let value = header;
    div()
        .id(SharedString::from(format!("{id}-ask")))
        .role(gpui::Role::Status)
        .aria_label(SharedString::from(format!("{label}: {value}")))
        .py(px(4.0))
        .w_full()
        .child(
            div()
                .h(px(34.0))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .rounded(px(TRANSCRIPT_CARD_RADIUS))
                .border_1()
                .border_color(theme.border)
                .bg(crate::theme::ink(0.045))
                .px(px(8.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(20.0))
                        .rounded(px(6.0))
                        .bg(crate::theme::ink(0.09))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            crate::icons::icon(icon)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(theme.text.opacity(0.9))
                        .child(value),
                ),
        )
        .into_any_element()
}

/// slopty: the chip's label for an agent's own questions.
const QUESTION_CHIP: &str = "Question";

/// slopty: an ask's chip words: its kind's label, and what it shows beside it. An agent's
/// questions show their header; an approval shows what it asks to run or change (its first
/// line); any other ask (a dialog, a form) shows its title or its first line, so a dialog never
/// reads as a question nobody can see.
fn input_chip_words(questions: &[slopty_view_proto::UserInputQuestion]) -> (SharedString, SharedString) {
    let Some(first) = questions.first() else {
        return (QUESTION_CHIP.into(), QUESTION_CHIP.into());
    };
    let kind = first.kind.as_str();
    let label = match kind {
        "approval" | "retry" => "Approval",
        "plan" => "Plan",
        "elicitation" => "Form",
        k if k.starts_with("dialog:") => "Confirm",
        _ => QUESTION_CHIP,
    };
    let header = first.header.trim();
    let asked = first.question.lines().map(str::trim).find(|line| !line.is_empty());
    // slopty: an approval shows what it would run or change ("touch refused.txt"), not its
    // generic header ("Bash command"), which says only the tool (#8): its command where it
    // runs one, which its words then only describe.
    let approval = label == "Approval";
    let asked = first
        .command
        .as_deref()
        .and_then(|command| command.lines().map(str::trim).find(|line| !line.is_empty()))
        .filter(|_| approval)
        .or(asked);
    let value = if label == QUESTION_CHIP {
        if header.is_empty() { QUESTION_CHIP } else { header }
    } else if approval && let Some(asked) = asked {
        asked
    } else if header.is_empty() || header.eq_ignore_ascii_case(label) {
        asked.unwrap_or(label)
    } else {
        header
    };
    (label.into(), single_line(value).into())
}

/// A small glyph standing in for the tool's icon (slopty uses an icon set; a
/// quiet monochrome character keeps the tile without shipping SVGs).
/// The glyph for a tool call (slopty tool-chip.tsx `toolIcon`, Solar set).
fn tool_icon_path(call: &ToolCall) -> &'static str {
    match call {
        ToolCall::Exec { .. } => crate::icons::TERMINAL,
        ToolCall::ReadFile { .. } | ToolCall::ApplyPatch { .. } => crate::icons::DOCUMENT,
        ToolCall::WriteFile { .. } => crate::icons::DOCUMENT_ADD,
        ToolCall::EditFile { .. } => crate::icons::PEN,
        ToolCall::Search { .. } => crate::icons::MAGNIFER,
        ToolCall::Glob { .. } => crate::icons::FOLDER_WITH_FILES,
        ToolCall::WebFetch { .. } | ToolCall::WebSearch { .. } => crate::icons::GLOBAL,
        ToolCall::Todo { .. } => crate::icons::CHECKLIST,
        call if is_agent_call(call) => crate::icons::BOT,
        ToolCall::Unknown { name, .. } if name == "Wait for agents" => crate::icons::BOT,
        ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => crate::icons::WIDGET,
    }
}

/// Compact file-action chips show only the final path component. The full
/// path remains on the tool call (and in expanded diagnostics) for identity
/// and disambiguation. Accept both separator styles because remote tools can
/// report Windows paths even when the UI is running elsewhere.
fn file_badge_name(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(path)
}

/// The body of an expanded chip card, under the header's separator. Diffs
/// render through the changes pane's section body — the real component, with
/// hunk headers, dual line-number gutters, accent bars, row washes, and
/// syntax runs — so an inline tool diff is indistinguishable from the
/// checkout diff sidebar. Output renders as a code block: verbatim mono
/// lines, indentation intact, counted-tail truncation.
fn detail_body(
    detail: &ToolDetail,
    diff_highlights: Option<Arc<crate::changes::DiffHighlights>>,
    selection_key: &str,
    theme: &Theme,
) -> AnyElement {
    let body = div().w_full().min_w_0().flex().flex_col().overflow_hidden();
    // Every line joins the transcript's selection, so paths, commands and
    // output can be drag-selected (across lines too) and copied. Lines clip
    // rather than ellipsize: selection maps the pointer onto the full text.
    let selectable = |line_ix: usize, text: SharedString, runs: Vec<TextRun>| {
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .child(render::selectable_text_element(
                format!("{selection_key}-{line_ix}").into(),
                text,
                runs,
                theme.selection,
            ))
    };
    let plain = |text: &SharedString, color: gpui::Hsla| {
        vec![TextRun {
            len: text.len(),
            font: gpui::font(theme.font_mono.clone()),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }]
    };
    match detail {
        // No comment layer: an inline tool diff is a record of what the
        // agent already did, not a review surface.
        ToolDetail::Diff { file, .. } => body
            .child(crate::changes::render_file_body_with_syntax(
                file,
                diff_highlights,
                theme,
            ))
            .into_any_element(),
        ToolDetail::Stats { stats } => body
            .py(px(6.0))
            .font_family(theme.font_mono.clone())
            .text_size(px(TOOL_TEXT_SIZE))
            .children(stats.iter().enumerate().map(|(stat_ix, stat)| {
                let path = SharedString::from(stat.path.clone());
                let runs = plain(&path, theme.text_faint);
                div()
                    .h(px(OUTPUT_LINE_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        crate::file_icons::icon(
                            crate::file_icons::FileIconIdentity::file(&stat.path),
                            theme.appearance,
                        )
                        .size(px(14.0))
                        .flex_none(),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(selectable(stat_ix, path, runs)),
                    )
                    // slopty: a file whose lines were never counted says no "+0 −0".
                    .when(stat.additions + stat.deletions > 0, |line| {
                        line.child(
                            div()
                                .flex_none()
                                .text_color(theme.success)
                                .child(SharedString::from(format!("+{}", stat.additions))),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.danger)
                                .child(SharedString::from(format!("−{}", stat.deletions))),
                        )
                    })
            }))
            .into_any_element(),
        // slopty: the block says its lines to assistive tech, as a markdown block says its words.
        ToolDetail::Output {
            lines,
            truncated_by,
            stderr_from,
        } => body
            .id(SharedString::from(format!("{selection_key}-output")))
            .role(gpui::Role::Code)
            .aria_label(SharedString::from(lines.join("\n")))
            .py(px(6.0))
            .font_family(theme.font_mono.clone())
            .text_size(px(TOOL_TEXT_SIZE))
            .children(lines.iter().enumerate().map(|(line_ix, line)| {
                // slopty: standard error reads in the danger colour, as the TUI's.
                let color = if stderr_from.is_some_and(|from| line_ix >= from) {
                    theme.danger
                } else {
                    theme.text_faint
                };
                let row = div()
                    .h(px(OUTPUT_LINE_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .text_color(color);
                if line.is_empty() {
                    return row.into_any_element();
                }
                // slopty: a URL in the output is a link, as the TUI makes it one.
                let Some((start, url)) = line_url(line) else {
                    return row
                        .child(selectable(line_ix, line.clone(), plain(line, color)))
                        .into_any_element();
                };
                let run = |len: usize, underline: Option<gpui::UnderlineStyle>| TextRun {
                    len,
                    font: gpui::font(theme.font_mono.clone()),
                    color,
                    background_color: None,
                    underline,
                    strikethrough: None,
                };
                let end = start.saturating_add(url.len());
                let link = gpui::UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(color),
                    wavy: false,
                };
                let mut runs = vec![
                    run(start, None),
                    run(url.len(), Some(link)),
                    run(line.len().saturating_sub(end), None),
                ];
                runs.retain(|run| run.len > 0);
                let url = url.to_owned();
                row.id(SharedString::from(format!("{selection_key}-link-{line_ix}")))
                    .role(gpui::Role::Link)
                    .aria_label(SharedString::from(url.clone()))
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&url);
                    })
                    .child(selectable(line_ix, line.clone(), runs))
                    .into_any_element()
            }))
            .when(*truncated_by > 0, |block| {
                block.child(more_lines_row(*truncated_by, theme))
            })
            .into_any_element(),
        ToolDetail::Thought {
            lines,
            truncated_by,
        } => body
            .py(px(6.0))
            .text_size(px(TOOL_TEXT_SIZE))
            .children(lines.iter().enumerate().map(|(line_ix, line)| {
                let row = div()
                    .h(px(OUTPUT_LINE_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center();
                let Some((text, runs)) = thought_line_text(line, theme) else {
                    return row; // blank separator row
                };
                row.child(selectable(line_ix, text, runs))
            }))
            .when(*truncated_by > 0, |block| {
                block.child(more_lines_row(*truncated_by, theme))
            })
            .into_any_element(),
    }
}

/// The counted-tail row under a truncated Output/Thought detail.
fn more_lines_row(truncated_by: usize, theme: &Theme) -> gpui::Div {
    div()
        .h(px(OUTPUT_LINE_HEIGHT))
        .flex()
        .items_center()
        .text_size(px(TOOL_TEXT_SIZE))
        .text_color(theme.text_faint)
        .child(SharedString::from(format!("… {truncated_by} more lines")))
}

/// Shape one flattened thought line into gpui text runs — the detail-body
/// palette: faint foreground prose, semibold for bold, mono for code,
/// underlined links (NOT clickable — a thought is a record, not a surface).
fn thought_line_text(line: &[InlineRun], theme: &Theme) -> Option<(SharedString, Vec<TextRun>)> {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    for run in line {
        if run.text.is_empty() {
            continue;
        }
        let mut f = if run.style.code {
            gpui::font(theme.font_mono.clone())
        } else {
            gpui::font(theme.font_sans_fixed.clone())
        };
        if run.style.bold {
            f.weight = gpui::FontWeight::SEMIBOLD;
        }
        if run.style.italic {
            f.style = gpui::FontStyle::Italic;
        }
        runs.push(TextRun {
            len: run.text.len(),
            font: f,
            color: theme.text_faint,
            background_color: None,
            underline: run.style.link.is_some().then_some(gpui::UnderlineStyle {
                color: Some(theme.text_faint),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: run.style.strikethrough.then_some(gpui::StrikethroughStyle {
                thickness: px(1.0),
                color: Some(theme.text_faint),
            }),
        });
        text.push_str(&run.text);
    }
    if text.trim().is_empty() {
        return None;
    }
    Some((text.into(), runs))
}

/// The trailing tile on a chip header, when it has one.
enum ChipTrail {
    /// Expand/collapse chevron — flipped while the detail body is open. slopty: on an activity
    /// row it shows as much as `shown`, its row's hover (Amp).
    Chevron { open: bool, shown: f32 },
    // slopty: no open-arrow; a subagent's card has its own (`transcript/subagents.rs`).
}

/// Opens a tool chip's file in the files panel through the transcript's
/// workspace-link handler.
#[derive(Clone)]
struct ToolFileOpener {
    link: render::LinkUi,
}

impl ToolFileOpener {
    /// Only paths the link handler resolves — inside one of the chat's roots,
    /// or an absolute host file it opens read-only — get a clickable badge;
    /// the rest stay inert instead of looking clickable and doing nothing.
    fn opens(&self, path: &str) -> bool {
        self.link.file_link(path).is_some()
    }

    fn open(&self, path: &str, window: &mut Window, cx: &mut gpui::App) {
        render::activate_link(
            render::LinkTarget::new(file_badge_name(path), path),
            render::LinkAction::Primary,
            Some(&self.link),
            window,
            cx,
        );
    }
}

/// The chip's content row: icon tile + label + detail line (+ trailing tile
/// when the chip expands or links out). Shared between the plain chip, the
/// header of an expandable chip card, and the spawn link chip.
///
/// Spawn chips carry their subagent's lifecycle VISUALLY, in the chip's own
/// language: while running the mini working spinner (the sidebar's) pulses
/// at the right of the ordinary static detail; done is the ordinary quiet
/// chip; failed takes the danger tint — no status words, no live text (a
/// header rewriting itself per stream delta read as noise — user report).
/// slopty: a call's words as its row draws them: its verb and its target ("Read",
/// "src/main.rs"), in the TUI's words where the call has them.
fn chip_words(tool: &ToolItem) -> (String, String) {
    let (label, detail) = match tool.kind {
        ToolItemKind::Thought => ("Thought process", String::new()),
        ToolItemKind::Call => tool_chip_content(&tool.call),
    };
    // slopty: a call zeron draws as `Tool · {name}` says itself in the TUI's words.
    let (label, detail) = match tool.facts.as_ref().and_then(|facts| facts.title.as_deref()) {
        Some(title) if tool.kind == ToolItemKind::Call => (title.to_owned(), String::new()),
        _ => (label.to_owned(), detail),
    };
    // slopty: a file's path as the TUI shows it: relative to the chat's folder, its middle cut.
    let detail = match tool.facts.as_ref().and_then(|facts| facts.detail.as_deref()) {
        Some(path) if tool.kind == ToolItemKind::Call && !detail.is_empty() => path.to_owned(),
        _ => detail,
    };
    (label, detail)
}

/// slopty: a call's row as assistive tech names it: "{verb} {target}" (#10).
fn chip_name(tool: &ToolItem) -> SharedString {
    let (label, detail) = chip_words(tool);
    let detail = single_line(&detail);
    if detail.is_empty() { label.into() } else { format!("{label} {detail}").into() }
}

/// slopty: what a call's row says it came to ("exit code 2"), for assistive tech (#10).
fn chip_result(tool: &ToolItem) -> Option<SharedString> {
    tool.facts.as_ref().and_then(|facts| facts.result.clone()).map(SharedString::from)
}

/// slopty: a call's row as assistive tech reads it: named by its call, valued by its result,
/// a list item, or a button when it opens (#10).
fn chip_a11y(row: gpui::Stateful<gpui::Div>, tool: &ToolItem, open: Option<bool>) -> gpui::Stateful<gpui::Div> {
    let row = row
        .role(if open.is_some() { gpui::Role::Button } else { gpui::Role::ListItem })
        .aria_label(chip_name(tool));
    let row = match chip_result(tool) {
        Some(result) => row.aria_value(result),
        None => row,
    };
    match open {
        Some(open) => row.aria_expanded(open),
        None => row,
    }
}

/// slopty: whether a call's row says its failure across its words. The person's own Deny ended
/// the call; it did not fail, so it reads quiet. An activity row's failure says so in its rail
/// icon and its result words ("exit code 2"), never across the whole line, which read as the
/// loudest thing on screen for a failure the agent expected (#7); a card, with no rail, keeps
/// the tint.
fn fails_loudly(tool: &ToolItem, activity: bool) -> bool {
    let failed = (tool.is_error && !denied(tool))
        || (tool.subagent_ref.is_some()
            && matches!(tool.subagent_status, Some(SubagentStatus::Failed)));
    failed && !activity
}

fn chip_header_row(
    tool: &ToolItem,
    trail: Option<ChipTrail>,
    file_opener: Option<&ToolFileOpener>,
    facts: Option<&work::ChipFacts>,
    shimmer: Option<f32>,
    theme: &Theme,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> gpui::Div {
    let (label, detail) = chip_words(tool);
    // slopty: a call under way sweeps its verb with its group title's light, in phase (#14).
    let shimmer = shimmer.filter(|_| sweeps(tool));
    let facts_trail = facts.zip(tool.facts.as_deref()).and_then(|(chip, facts)| {
        chip.trail(&tool.part_id, facts, tool.resolved, theme)
    });
    let activity = !is_agent_tool(tool);
    let file_path = match &tool.call {
        ToolCall::ReadFile { path }
        | ToolCall::WriteFile { path, .. }
        | ToolCall::EditFile { path, .. }
        | ToolCall::ApplyPatch { path: Some(path) } => Some(path.as_str()),
        _ => None,
    };
    let open_file = file_path
        .zip(file_opener)
        .filter(|(path, opener)| opener.opens(path))
        .map(|(path, opener)| (path.to_owned(), opener.clone()));
    let running = tool.subagent_ref.is_some()
        && matches!(tool.subagent_status, Some(SubagentStatus::Running));
    let failed = fails_loudly(tool, activity);
    // Text resolves its color during layout, so group-hover text needs stable
    // child IDs under the keyed, expandable header to retain hover state.
    let hover_text = activity && trail.is_some();
    let tint = if failed { theme.danger } else { theme.text_muted };
    div()
        .group("tool-header")
        .h(px(if activity {
            CHIP_CARD_HEIGHT
        } else {
            CHIP_HEADER_HEIGHT
        }))
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(if activity { 0.0 } else { 8.0 }))
        .text_size(px(TOOL_LABEL_SIZE))
        .line_height(px(TOOL_LABEL_LINE_HEIGHT))
        .when(!activity, |row| {
            row.child(
                // Subagent icon tile (`size-[18px] rounded-[5px] bg-white/[0.08]`,
                // icon size-3).
                div()
                    .size(px(18.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .bg(crate::theme::ink(0.08))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        crate::icons::icon(match tool.kind {
                            ToolItemKind::Thought => crate::icons::CHAT_ROUND_LINE,
                            ToolItemKind::Call => tool_icon_path(&tool.call),
                        })
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                    ),
            )
        })
        .child(
            div()
                .flex_none()
                .h(px(TOOL_LABEL_LINE_HEIGHT))
                .flex()
                .items_center()
                .when(!activity, |label| {
                    label.font_weight(gpui::FontWeight::MEDIUM)
                })
                .text_color(tint)
                .child(tool_group_title(SharedString::from(label), &[], shimmer, theme))
                .map(|label| {
                    if hover_text {
                        label
                            .id("tool-label")
                            .group_hover("tool-header", |style| style.text_color(theme.text))
                            .into_any_element()
                    } else {
                        label.into_any_element()
                    }
                }),
        )
        .child(
            div()
                .when(!activity, |detail| detail.flex_1())
                .min_w_0()
                .h(px(if file_path.is_some() {
                    22.0
                } else {
                    TOOL_LABEL_LINE_HEIGHT
                }))
                .flex()
                .when(activity && detail.is_empty(), |detail| detail.hidden())
                .items_center()
                .truncate()
                .text_color(if failed {
                    theme.danger
                } else if activity {
                    theme.text_muted
                } else {
                    theme.text.opacity(0.85)
                })
                .child(if let Some(path) = file_path {
                    let badge = div()
                        .min_w_0()
                        .h(px(22.0))
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .gap(px(6.0))
                        .rounded(px(5.0))
                        .bg(theme.ink(0.06))
                        .pl(px(1.0))
                        .pr(px(6.0))
                        .text_color(theme.text.opacity(0.85))
                        .child(
                            div()
                                .size(px(20.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.0))
                                .bg(crate::file_icons::well_bg(theme))
                                .child(
                                    crate::file_icons::icon(
                                        crate::file_icons::FileIconIdentity::file(path),
                                        theme.appearance,
                                    )
                                    .size(px(14.0)),
                                ),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(file_badge_name(path).to_owned())),
                        )
                        .map(|badge| {
                            if !hover_text && open_file.is_none() {
                                return badge.into_any_element();
                            }
                            badge
                                .id("tool-file-badge")
                                .when(hover_text, |badge| {
                                    badge.group_hover("tool-header", |style| {
                                        style.text_color(theme.text)
                                    })
                                })
                                // The badge is its own target: opening the
                                // file must not also toggle the chip's
                                // accordion underneath it.
                                .when_some(open_file, |badge, (path, opener)| {
                                    badge
                                        .debug_selector(|| "tool-file-badge".into())
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style.bg(theme.ink(0.1)).text_color(theme.text)
                                        })
                                        .on_click(move |_, window, cx| {
                                            cx.stop_propagation();
                                            opener.open(&path, window, cx);
                                        })
                                })
                                .into_any_element()
                        });
                    crate::frost::frosted(5.0, 16.0, badge).into_any_element()
                } else {
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(detail))
                        .into_any_element()
                })
                .map(|detail| {
                    if hover_text {
                        detail
                            .id("tool-detail")
                            .group_hover("tool-header", |style| style.text_color(theme.text))
                            .into_any_element()
                    } else {
                        detail.into_any_element()
                    }
                }),
        )
        .children(facts_trail) // slopty: its result, timer, task and "Run in background".
        .when_some(tool.call.subagent_model(), |row, model| {
            // Which model the child runs on, when the spawn named one.
            //
            // In the trailing slot rather than suffixed onto the detail: the
            // detail is the truncating slot, and the model is exactly what a
            // reader scanning a fan-out of spawns wants left once the
            // descriptions are cut.
            //
            // Bare faint text, NOT a filled pill: the tiles either side of it
            // are AFFORDANCES (the spinner means running, the arrow opens the
            // subagent), so giving a passive label the same chrome made the
            // trailing edge read as three buttons — the loudest thing in the
            // row was the one thing you cannot click.
            row.child(
                div()
                    .flex_none()
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(model.to_owned())),
            )
        })
        .when(running, |row| {
            // The sidebar working-row spinner, in the chip's trailing slot —
            // paint-local (fixed footprint), so it never moves the layout.
            row.child(div().flex_none().child(crate::loaders::mini_glyph_spinner(
                format!(
                    "subagent-chip-{}",
                    tool.subagent_ref.as_deref().unwrap_or_default()
                ),
                2.0,
                theme.glyph,
                view,
                cx,
            )))
        })
        .when_some(trail, |row, trail| {
            let ChipTrail::Chevron { shown, .. } = trail;
            // Trailing tile matching the group header's: a chevron for the
            // output/diff accordion, or the open-arrow for spawn chips.
            let tile = div()
                .size(px(18.0))
                .flex_none()
                .when(activity, |tile| tile.opacity(shown))
                .when(!activity, |tile| {
                    tile.rounded(px(5.0)).bg(crate::theme::ink(0.06))
                })
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_muted.opacity(0.8));
            row.child(match trail {
                ChipTrail::Chevron { open, .. } => tile.child(
                    crate::icons::icon(if open {
                        crate::icons::ALT_ARROW_DOWN
                    } else {
                        crate::icons::ALT_ARROW_RIGHT
                    })
                    .size(px(12.0))
                    .text_color(theme.text_faint)
                    .when(activity, |caret| {
                        caret.group_hover("tool-header", |style| style.text_color(theme.text))
                    }),
                ),
            })
        })
}

/// The header row of an expandable chip card.
fn chip_header(
    tool: &ToolItem,
    open: bool,
    shown: f32,
    file_opener: Option<&ToolFileOpener>,
    facts: Option<&work::ChipFacts>,
    shimmer: Option<f32>,
    theme: &Theme,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> gpui::Div {
    chip_header_row(
        tool,
        Some(ChipTrail::Chevron { open, shown }),
        file_opener,
        facts,
        shimmer,
        theme,
        view,
        cx,
    )
}

/// Max chars a subagent tab title keeps. The strip chip is fixed-width and
/// truncates visually, but the derived title also rides drag ghosts and any
/// future pickers — cap it at the source.
const SUBAGENT_TITLE_MAX: usize = 40;

/// First line of `text`, trimmed, capped at `max` chars with an ellipsis.
fn title_line(text: &str, max: usize) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    Some(out)
}

/// Drop a leading "Agent"/"Task" genus (with its `:` and spacing) from a
/// spawn-title candidate. Only a real word boundary strips — "Taskmaster"
/// keeps its name. A bare "Agent"/"Task" strips to "" (no context at all).
fn strip_spawn_prefix(text: &str) -> &str {
    let t = text.trim();
    for prefix in ["agent", "task"] {
        if t.len() >= prefix.len()
            && t.is_char_boundary(prefix.len())
            && t[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            let rest = &t[prefix.len()..];
            if rest.is_empty() {
                return "";
            }
            if rest.starts_with(':') || rest.starts_with(char::is_whitespace) {
                return rest.trim_start_matches(':').trim();
            }
        }
    }
    t
}

/// Tab title for a spawn chip's subagent surface: the BARE task description
/// ("verify the marker pipeline"). The chip keeps the tool's fuller name —
/// a fixed-width tab spent on "Agent: " never shows the task, so the genus
/// is stripped here and the call input's description/prompt fields back up
/// a bare name (older docs); "Subagent" only as the last resort.
pub(crate) fn subagent_tab_title(call: &ToolCall) -> SharedString {
    let (name, input) = match call {
        ToolCall::Unknown { name, input } => (name.as_str(), input.as_ref()),
        ToolCall::Mcp { tool, input, .. } => (tool.as_str(), input.as_ref()),
        _ => return "Subagent".into(),
    };
    let candidates = [
        Some(name),
        input.and_then(|i| i.get("description")?.as_str()),
        input.and_then(|i| i.get("prompt")?.as_str()),
    ];
    for text in candidates.into_iter().flatten() {
        if let Some(title) = title_line(strip_spawn_prefix(text), SUBAGENT_TITLE_MAX) {
            return title.into();
        }
    }
    "Subagent".into()
}

/// Clip one newly appended task row to its committed height. Fade and lift are
/// applied to the row content itself, leaving the connector at full contrast
/// while it draws; fading the whole row made the path animation imperceptible.
fn reveal_tool_row(row: AnyElement, height: f32, progress: f32) -> AnyElement {
    if progress >= 1.0 {
        return row;
    }
    div()
        .w_full()
        .h(px(height * progress))
        .flex_none()
        .overflow_hidden()
        .child(row)
        .into_any_element()
}

/// slopty: a touch screen, where nothing hovers.
/// slopty: how far row `key`'s message strip has faded toward `shown` at `now`, and whether it
/// still moves, remembered in `fades`. `still` (reduced motion, or a touch screen, which shows it
/// at rest) snaps it.
fn strip_fade(
    fades: &mut HashMap<SharedString, (bool, f32, Instant)>,
    key: &SharedString,
    shown: bool,
    now: Instant,
    still: bool,
) -> (f32, bool) {
    let target = |on: bool| if on { 1.0 } else { 0.0 };
    if still {
        fades.remove(key);
        return (target(shown), false);
    }
    let at = |(on, from, since): (bool, f32, Instant)| {
        let total = STRIP_FADE.total().mul_f32(motion::speed_scale()).as_secs_f32();
        let progress = STRIP_FADE.progress(now.saturating_duration_since(since).as_secs_f32() / total.max(f32::EPSILON));
        (motion::lerp(from, target(on), progress), progress < 1.0)
    };
    let fade = match fades.get(key).copied() {
        None if !shown => return (0.0, false),
        None => (true, 0.0, now),
        Some(fade) if fade.0 != shown => (shown, at(fade).0, now),
        Some(fade) => fade,
    };
    let (t, moving) = at(fade);
    if !shown && !moving {
        fades.remove(key);
    } else {
        fades.insert(key.clone(), fade);
    }
    (t, moving)
}

const TOUCH: bool = cfg!(target_os = "ios");
/// slopty: a message strip's fade as the pointer comes and goes (Amp's 200 ms, on the
/// Tailwind ease the hover washes ride).
const STRIP_FADE: motion::MotionSpec = motion::MotionSpec::new(200, motion::EASE_TAILWIND);

/// slopty: whether the person turned the call down (their own Deny): an end, not a failure.
fn denied(tool: &ToolItem) -> bool {
    tool.facts.as_ref().is_some_and(|facts| facts.rejected)
}

/// BoardUI-style task tree with Slopty's tool glyph restored at each branch tip.
/// The previous row draws the first leg of a new arrival to its lower boundary;
/// this row then continues down, rounds the elbow, and finally reveals the icon.
/// One paint owns every segment in a row, avoiding alpha-darkened joints.
fn activity_rail(
    tool: &ToolItem,
    has_predecessor: bool,
    continues: bool,
    reveal: f32,
    continuation_reveal: f32,
    row_height: f32,
    theme: &Theme,
) -> gpui::Div {
    let color = theme.hairline(0.12);
    let tint = if tool.is_error && !denied(tool) {
        theme.danger
    } else {
        theme.text_muted
    };
    let (incoming_reveal, branch_reveal) = tool_connector_parts(reveal, has_predecessor);
    div()
        .relative()
        .w(px(ACTIVITY_GUTTER_WIDTH))
        .flex_none()
        .child(
            canvas(
                move |_, _, _| (),
                move |bounds, _, window, _| {
                    let x = bounds.origin.x + px(ACTIVITY_TRUNK_X);
                    let branch_y = bounds.origin.y + px(row_height / 2.0);
                    let bend_y = branch_y - px(ACTIVITY_BEND_RADIUS);
                    // Union the ribbons before painting. Stroke tessellation
                    // blends intersections twice, even in a single path.
                    let mut tree = PathBuilder::fill().with_style(gpui::PathStyle::Fill(
                        gpui::FillOptions::default().with_fill_rule(gpui::FillRule::NonZero),
                    ));
                    if incoming_reveal > 0.0 {
                        let mut bottom = point(
                            x,
                            bounds.origin.y
                                + px((row_height / 2.0 - ACTIVITY_BEND_RADIUS) * incoming_reveal),
                        );
                        if incoming_reveal >= 1.0 && continues && continuation_reveal > 0.0 {
                            let continuation_height = (f32::from(bounds.size.height)
                                - (row_height / 2.0 - ACTIVITY_BEND_RADIUS))
                                .max(0.0);
                            bottom =
                                point(x, bend_y + px(continuation_height * continuation_reveal));
                        }
                        activity_ribbon(&mut tree, &[point(x, bounds.origin.y), bottom]);
                    }
                    if branch_reveal > 0.0 {
                        let points: Vec<_> = activity_branch_points(branch_reveal)
                            .into_iter()
                            .map(|p| point(x + px(p.x), bend_y + px(p.y)))
                            .collect();
                        activity_ribbon(&mut tree, &points);
                    }
                    if let Ok(path) = tree.build() {
                        window.paint_path(path, color);
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(
            crate::icons::icon(match tool.kind {
                ToolItemKind::Thought => crate::icons::CHAT_ROUND_LINE,
                ToolItemKind::Call => tool_icon_path(&tool.call),
            })
            .absolute()
            .left(px(ACTIVITY_ICON_LEFT))
            .top(px(row_height / 2.0 - ACTIVITY_ICON_SIZE / 2.0))
            .size(px(ACTIVITY_ICON_SIZE))
            .opacity(branch_reveal)
            .text_color(tint),
        )
}

/// A clockwise ribbon contour. Nonzero fill unions intersecting contours,
/// preserving one alpha contribution at the fork on transparent surfaces.
fn activity_ribbon(path: &mut PathBuilder, points: &[Point<Pixels>]) {
    let mut left = Vec::with_capacity(points.len());
    let mut right = Vec::with_capacity(points.len());
    for (ix, p) in points.iter().enumerate() {
        let a = points[ix.saturating_sub(1)];
        let b = points[(ix + 1).min(points.len() - 1)];
        let dx = f32::from(b.x - a.x);
        let dy = f32::from(b.y - a.y);
        let length = dx.hypot(dy).max(0.0001);
        let normal = point(px(-dy / length * 0.5), px(dx / length * 0.5));
        left.push(*p + normal);
        right.push(*p - normal);
    }
    path.move_to(left[0]);
    for p in left.iter().skip(1).chain(right.iter().rev()) {
        path.line_to(*p);
    }
    path.close();
}

/// A plain activity row, or a card for a subagent without a linked document.
/// slopty: `id` keys it for assistive tech, which reads it named by its call (#10); a lone call
/// (`solo`) wears its icon on the summary's chevron column, its words on the summaries' edge
/// (#2); `shimmer` sweeps a running call's verb (#14).
fn tool_chip(
    id: SharedString,
    tool: &ToolItem,
    rail: bool,
    solo: bool,
    has_predecessor: bool,
    continues: bool,
    content_reveal: f32,
    connector_reveal: f32,
    continuation_reveal: f32,
    file_opener: Option<&ToolFileOpener>,
    facts: Option<&work::ChipFacts>,
    shimmer: Option<f32>,
    theme: &Theme,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> AnyElement {
    let row_height = if rail {
        TOOL_TREE_ROW_HEIGHT
    } else {
        CHIP_HEIGHT
    };
    let row = div().id(id);
    let row = if is_agent_tool(tool) { row } else { chip_a11y(row, tool, None) };
    row.h(px(row_height))
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .when(rail, |row| {
            row.child(if solo {
                solo_rail(tool, row_height, theme)
            } else {
                activity_rail(
                    tool,
                    has_predecessor,
                    continues,
                    connector_reveal,
                    continuation_reveal,
                    row_height,
                    theme,
                )
            })
        })
        .child(
            div()
                .when(rail, |el| el.ml(px(activity_text_gap(solo))))
                .my(px((row_height - CHIP_CARD_HEIGHT) / 2.0))
                .h(px(CHIP_CARD_HEIGHT))
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .overflow_hidden()
                .when(!rail, |card| {
                    card.rounded(px(TRANSCRIPT_CARD_RADIUS)) // slopty: the column's card (#19).
                        .border_1()
                        .border_color(theme.border)
                        .bg(crate::theme::ink(0.03))
                })
                .when(rail && content_reveal < 1.0, |card| {
                    card.relative()
                        .top(px(4.0 * (1.0 - content_reveal)))
                        .opacity(content_reveal)
                })
                .child(chip_header_row(tool, None, file_opener, facts, shimmer, theme, view, cx)),
        )
        .into_any_element()
}

/// slopty: whether `tool`'s row sweeps its words with its group title's light: a call under way,
/// not a thought or a subagent, whose card says it runs (#14).
fn sweeps(tool: &ToolItem) -> bool {
    !tool.resolved && tool.kind == ToolItemKind::Call && !is_spawn_link(tool)
}

/// slopty: where an activity row's words start past its rail: after the tree's gutter, or, for
/// a lone call, on the summaries' edge (#2).
fn activity_text_gap(solo: bool) -> f32 {
    if solo { ACTIVITY_TITLE_X - SOLO_RAIL_WIDTH } else { ACTIVITY_TEXT_GAP }
}

/// slopty: a lone call's rail: no tree, its icon centred on the summary chevron's column (#2).
const SOLO_RAIL_WIDTH: f32 = 22.0;

fn solo_rail(tool: &ToolItem, row_height: f32, theme: &Theme) -> gpui::Div {
    let tint = if tool.is_error && !denied(tool) {
        theme.danger
    } else {
        theme.text_muted
    };
    div().relative().w(px(SOLO_RAIL_WIDTH)).flex_none().child(
        crate::icons::icon(match tool.kind {
            ToolItemKind::Thought => crate::icons::CHAT_ROUND_LINE,
            ToolItemKind::Call => tool_icon_path(&tool.call),
        })
        .absolute()
        .left(px(ACTIVITY_TRUNK_X - ACTIVITY_ICON_SIZE / 2.0))
        .top(px(row_height / 2.0 - ACTIVITY_ICON_SIZE / 2.0))
        .size(px(ACTIVITY_ICON_SIZE))
        .text_color(tint),
    )
}

fn entry_fingerprint(entry: &SessionMessageEntry, pending: bool) -> u64 {
    let mut acc: Vec<u8> = Vec::with_capacity(entry.parts.len() * 8 + 16);
    acc.extend_from_slice(entry.id.as_bytes());
    acc.extend_from_slice(entry.device_id.as_bytes());
    acc.push(match entry.status {
        None => 0,
        Some(MessageStatus::Streaming) => 1,
        Some(MessageStatus::Complete) => 2,
        Some(MessageStatus::Aborted) => 3,
    });
    acc.push(pending as u8);
    acc.extend_from_slice(&entry.duration_ms.unwrap_or(0).to_le_bytes());
    for part in &entry.parts {
        acc.extend_from_slice(part.id().as_bytes());
        acc.extend_from_slice(&(part.byte_len() as u64).to_le_bytes());
        if let MessagePart::Tool {
            is_error,
            resolved,
            subagent_ref,
            subagent_status,
            subagent_tail,
            ..
        } = part
        {
            acc.push(*is_error as u8 | (*resolved as u8) << 1);
            // Subagent lifecycle mutates a COMPLETED entry in place (eager-
            // done: the spawn resolves while the subagent runs on) and
            // `byte_len` above doesn't cover these fields — hash them or the
            // cached rows never refresh on status/tail changes.
            acc.push(
                subagent_ref.is_some() as u8
                    | match subagent_status {
                        None => 0,
                        Some(SubagentStatus::Running) => 1 << 1,
                        Some(SubagentStatus::Done) => 2 << 1,
                        Some(SubagentStatus::Failed) => 3 << 1,
                    },
            );
            if let Some(tail) = subagent_tail {
                acc.extend_from_slice(tail.as_bytes());
            }
        }
        if let MessagePart::Image {
            path,
            name,
            mime_type,
            ..
        } = part
        {
            for field in [path, name, mime_type] {
                acc.extend_from_slice(field.as_bytes());
                acc.push(0);
            }
        }
        if let MessagePart::Visual { path, title, .. } = part {
            for field in [path, title] {
                acc.extend_from_slice(field.as_bytes());
                acc.push(0);
            }
        }
        if let MessagePart::Input { resolved, .. } = part {
            acc.push(0x10 | *resolved as u8);
        }
        // slopty: an error's and a divider's words change in place.
        match part {
            MessagePart::Error { message, .. } => {
                acc.push(0x20);
                acc.extend_from_slice(message.as_bytes());
            }
            MessagePart::Marker { text, .. } => acc.extend_from_slice(text.as_bytes()),
            // slopty: facts, a task's notice, a notice and a page change in place.
            MessagePart::Tool { facts: Some(facts), .. } => work::hash_into(&mut acc, facts),
            MessagePart::Task { task, .. } => work::hash_into(&mut acc, task),
            MessagePart::Note { note, .. } => work::hash_into(&mut acc, note),
            MessagePart::Page { card, .. } => work::hash_into(&mut acc, card),
            _ => {}
        }
    }
    fnv1a(&acc)
}

impl Render for Transcript {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // slopty: before the list lays out, where reading it borrows nothing.
        self.page_viewport = self.list.viewport_bounds();
        // slopty: a subagent's card sizes its child by the window, and drills in on a phone's.
        self.subagents.window = window.viewport_size();
        // slopty: the list stopped following on an input of its own (its scrollbar's drag), so
        // the rule lets go too.
        if self.pinned && !self.list.is_following_tail() {
            self.let_go();
        }
        self.md_images.borrow_mut().folder = self.image_host(cx).1;
        if record_view_frame("transcript") {
            tracing::warn!(
                distance = self.distance_from_bottom(),
                following = self.pinned,
                veils = self.veils.len(),
                "transcript motion state"
            );
        }
        self.render_cache
            .borrow_mut()
            .retain_rows(&self.rendered_rows);
        self.rendered_rows.clear();
        // slopty: a strip no longer drawn lets its focus go, unless it holds the keyboard (#13).
        let drawn = std::mem::take(&mut self.strip_drawn);
        self.strip_focus.retain(|entry, focus| {
            drawn.contains(entry) || focus.copy.is_focused(window)
        });
        let content_width =
            crate::settings::transcript_width(f32::from(window.viewport_size().width), cx);
        if self.content_width != content_width {
            self.content_width = content_width;
            // The outer list viewport may not resize when only max-width
            // changes. Invalidate virtual row heights explicitly, retaining
            // their anchors and all live animation/provenance state.
            self.list.remeasure();
        }
        let typography_generation = crate::typography::generation(cx);
        if self.typography_generation != typography_generation {
            self.typography_generation = typography_generation;
            // `refresh_windows` re-lays out visible rows, but ListState keeps
            // measured heights for virtualized rows outside the viewport.
            // Mark every row unmeasured while retaining height hints and a
            // proportional scroll anchor; GPUI will refresh each measurement
            // as the row enters its layout range.
            self.list.remeasure();
        }
        // Release gpui-side decoded copies of any images the attachment LRU
        // evicted since the last frame (no-op when nothing was evicted).
        crate::attachments::flush_evicted(Some(window), cx);
        // Diagram requests are scoped to paint passes; rasters follow the
        // reading column and display density (a no-op while both hold).
        let released_diagrams = {
            let mut diagrams = self.diagrams.borrow_mut();
            let mut released = diagrams.begin_frame(crate::theme::style_generation());
            let list_width = f32::from(self.list.viewport_bounds().size.width);
            let column = if list_width > 0.0 {
                self.content_width
                    .min((list_width - self.right_reserve).max(1.0))
            } else {
                self.content_width
            };
            released.extend(diagrams.set_view((column.max(1.0), 480.0), window.scale_factor()));
            released
        };
        if !released_diagrams.is_empty() {
            crate::image_media::release_media(released_diagrams, cx);
        }
        let frame_asked_at = crate::clock::now(cx);
        // slopty: a jump's glide steps once a frame (#13).
        if self.glide.is_some()
            && let Some(ask) = self.glide_scheduled.ask(frame_asked_at)
        {
            let entity = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                entity
                    .update(cx, |this: &mut Transcript, cx| {
                        if this.glide_scheduled.answered(ask) {
                            this.step_glide(cx);
                        }
                    })
                    .ok();
            });
        }
        // Programmatic `scroll_to` does not invoke the list's user-scroll
        // handler. Refresh distance-derived state once layout has measured the
        // replay, guarded so a stale A callback cannot mutate B (or a newer A).
        if self.viewport_finalize_pending
            && let Some(ask) = self.viewport_finalize_scheduled.ask(frame_asked_at)
        {
            let token = ViewportFinalizeToken {
                generation: self.viewport_generation,
                layout_revision: self.viewport_layout_revision,
            };
            let entity = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                entity
                    .update(cx, |this: &mut Transcript, cx| {
                        if !this.viewport_finalize_scheduled.answered(ask) {
                            return;
                        }
                        if !token.still_current(this.viewport_generation) {
                            if this.viewport_finalize_pending {
                                cx.notify();
                            }
                            return;
                        }
                        this.last_scroll_distance = this.distance_from_bottom();
                        if token.layout_settled(this.viewport_layout_revision) {
                            this.viewport_finalize_pending = false;
                        }
                        cx.notify();
                    })
                    .ok();
            });
        }
        // A long-message collapse near the bottom owns the viewport for the
        // duration of its height tween. Advance the matching upward scroll once
        // per frame so the bubble stays visible instead of shrinking above the
        // fixed viewport while the bottom content remains on screen.
        if self.user_collapse_scroll.is_some()
            && let Some(ask) = self.user_collapse_scroll_scheduled.ask(frame_asked_at)
        {
            let entity = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                entity
                    .update(cx, |this: &mut Transcript, cx| {
                        if this.user_collapse_scroll_scheduled.answered(ask) {
                            this.step_user_collapse_scroll(cx);
                        }
                    })
                    .ok();
            });
        }
        let list_el = crate::overscroll::overscroll(
            "transcript-overscroll",
            crate::overscroll::OverscrollSource::List(self.list.clone()),
            list(self.list.clone(), cx.processor(Self::render_row))
                .size_full()
                .with_sizing_behavior(gpui::ListSizingBehavior::Auto),
        );
        // slopty: every transcript owns its scroll edge, inside its pane: top edge only (nothing
        // overlays the pane's bottom). Its ramp is as tall as what lies above, read at paint
        // (HextaUI's scroll area; design-inspiration item 4), so a transcript at its top shows
        // none and a scroll of 4 px dims 4 px, never a whole band at once.
        let scroll = self.list.clone();
        let content: AnyElement =
            crate::edge_fade::edge_faded(Theme::TRANSCRIPT_FADE_BAND, true, false, list_el)
                .fade_scroll_y_with(move |_| {
                    let max = f32::from(scroll.max_offset_for_scrollbar().y);
                    let above = (-f32::from(scroll.scroll_px_offset_for_scrollbar().y)).clamp(0.0, max.max(0.0));
                    (above, (max - above).max(0.0))
                })
                .into_any_element();
        let root = div()
            .relative()
            .size_full()
            .min_h_0()
            // slopty: every press starts off no footer's; the footer's own frame says otherwise
            // before the press handler below hears it.
            .capture_any_mouse_down(cx.listener(|this, _: &gpui::MouseDownEvent, _, _| this.footer_press = false))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(Self::on_selection_mouse_down),
            )
            .on_mouse_move(cx.listener(Self::on_selection_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_selection_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_selection_mouse_up))
            // slopty: a right-click on selected text opens its menu.
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_context_click))
            // FIRST child ⇒ paints first: clears this transcript's slice of the
            // frame's markdown text-selection registry before any row's text
            // elements re-register (document paint order = selection order;
            // see markdown/render.rs). Keyed by entity so a side chat or
            // subagent tab painted in the same frame can't wipe it.
            .child(crate::markdown::render::selection_frame_reset_for(
                cx.entity_id().as_u64(),
            ))
            // slopty: find's per-row counts start again each frame, before any row paints.
            .child(crate::markdown::find::frame_reset(cx.entity_id().as_u64()))
            .child(content)
            .children(self.fit.clone().map(|fit| fit.measure(self.list.clone()))) // slopty
            // slopty: with no row to end, the footer stands at the foot alone.
            .children(self.footer.clone().filter(|_| self.rows.is_empty()).map(|footer| {
                let footer = Self::footer_frame(footer, cx);
                div().absolute().left_0().right_0().bottom_0().px(px(self.row_inset())).child(footer)
            }));
        // slopty: the held group's band defers its paint above the top fade; added before the
        // menus, which defer theirs too, so they open over it.
        let root = root.children(self.render_sticky_group(window, cx));
        let root = root.children(self.render_image_menu(cx));
        let root = root.children(self.render_context_menu(cx)); // slopty
        // slopty: find in thread, over the transcript's top right.
        let root = root.children(self.render_find_bar(window, cx));
        let root = root.children(self.render_jump(cx));
        // Full-size viewer for a clicked user-bubble thumbnail
        // (AttachmentPreviewDialog: bare lightbox, click closes).
        if let Some(preview) = self.attachment_preview.clone() {
            let weak = cx.weak_entity();
            // A diagram's enlarged raster is framed by its natural size.
            let natural_size = self
                .diagram_zoom
                .as_ref()
                .map(|source| size(px(source.width), px(source.height)));
            return root.child(crate::attachments::lightbox_with_size(
                window,
                &preview,
                &self.attachment_preview_focus,
                natural_size,
                move |window, cx| {
                    if let Ok(focus) = weak.update(cx, |this, cx| {
                        this.close_diagram_zoom(cx);
                        this.attachment_preview = None;
                        cx.notify();
                        this.attachment_preview_return_focus.take()
                    }) && let Some(focus) = focus
                    {
                        window.focus(&focus, cx);
                    }
                },
                cx,
            ));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// slopty: the follow rule as a model (`docs/decisions/inspector.md`, "Follow and anchor"),
    /// over seeded runs of record arrivals (below the view, above it, and a reply streaming at
    /// the end) and the person's moves (a wheel up or down, a press in a row). An arrival never
    /// changes whether the view follows; following, the view stays at the end; not following,
    /// the first row in view keeps its key and its place to the pixel, whatever landed. Only the
    /// person lets go, and scrolling back to the end follows again.
    #[gpui::test]
    fn the_view_moves_only_when_the_person_moves_it(cx: &mut gpui::TestAppContext) {
        struct Root(Entity<Transcript>);
        impl Render for Root {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(self.0.clone())
            }
        }
        #[derive(Debug, Clone, Copy)]
        enum Step {
            Below(usize),
            Above(usize),
            Stream(usize),
            WheelUp(f32),
            WheelDown(f32),
            Press,
        }
        let paragraphs = |n: usize, tag: &str| (0..n).map(|i| format!("{tag} paragraph {i}.\n\n")).collect::<String>();
        let dir = tempfile::tempdir().unwrap();
        let state = cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.new(|_| AppState::new())
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let (_, cx) = cx.add_window_view(|_, _| Root(transcript.clone()));
        let mut entries: Vec<SessionMessageEntry> = (0..3)
            .map(|i| assistant(&format!("e{i}"), MessageStatus::Complete, vec![text_part("t", &paragraphs(30, "Old"))]))
            .collect();
        let show = |entries: &[SessionMessageEntry], cx: &mut gpui::VisualTestContext| {
            state.update(cx, |state, cx| {
                state.selected_chat = Some("chat".into());
                state.transcript_replayed = true;
                state.transcript = entries.to_vec();
                state.transcript_revision += 1;
                cx.notify();
            });
            cx.run_until_parked();
            for _ in 0..3 {
                cx.executor().advance_clock(Duration::from_millis(16));
                cx.update(|window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                });
            }
        };
        // Where the view stands: whether it follows, its distance from the end, and its first
        // row in view as drawn, with that row's top against the viewport's; a list the layout
        // holds at its very end says so ([`END`]), as it has no row to anchor by until rows land.
        const END: &str = "<end>";
        let probe = |cx: &mut gpui::VisualTestContext| {
            transcript.read_with(cx, |this, _| {
                let top = f32::from(this.list.viewport_bounds().top());
                let first = if this.is_glued() {
                    Some((SharedString::from(END), 0.0))
                } else {
                    (0..this.rows.len()).find_map(|ix| {
                        let bounds = this.list.bounds_for_item(ix)?;
                        (f32::from(bounds.bottom()) > top + 0.5)
                            .then(|| (this.rows[ix].id.clone(), f32::from(bounds.top()) - top))
                    })
                };
                // How the list holds itself, said with a failure.
                let held = format!(
                    "{:?} tail={} rows={} vp={}",
                    this.list.logical_scroll_top(),
                    this.list.is_following_tail(),
                    this.rows.len(),
                    f32::from(this.list.viewport_bounds().size.height),
                );
                (this.pinned, this.distance_from_bottom(), first, held)
            })
        };
        show(&entries, cx);
        let opened = probe(cx);
        assert!(opened.0, "a session opens following its end: {opened:?}");
        assert!(
            transcript.read_with(cx, |this, _| this.rows.len()) > 10,
            "its rows are in, more than a viewport of them: {opened:?}"
        );
        let middle = transcript.read_with(cx, |this, _| this.list.viewport_bounds().center());
        let mut seed: u64 = 0x5eed_f011_0b0e;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut streamed = 0;
        // The steps so far and where each left the view, said with a failure.
        let mut history: Vec<String> = Vec::new();
        for round in 0..160 {
            let step = match next(6) {
                0 => Step::Below(1 + next(12) as usize),
                1 => Step::Above(1 + next(6) as usize),
                2 => Step::Stream(1 + next(4) as usize),
                3 => Step::WheelUp(20.0 + next(600) as f32),
                4 => Step::WheelDown(20.0 + next(900) as f32),
                _ => Step::Press,
            };
            let before = probe(cx);
            match step {
                Step::Below(n) => {
                    let id = format!("e{}", entries.len());
                    entries.push(assistant(&id, MessageStatus::Complete, vec![text_part("t", &paragraphs(n, "New"))]));
                    show(&entries, cx);
                }
                Step::Above(n) => {
                    let MessagePart::Text { text, .. } = &mut entries[0].parts[0] else { unreachable!() };
                    text.push_str(&paragraphs(n, "Late"));
                    show(&entries, cx);
                }
                Step::Stream(n) => {
                    streamed += n;
                    entries.retain(|entry| entry.id != "live");
                    entries.push(assistant("live", MessageStatus::Streaming, vec![text_part("t", &paragraphs(streamed, "Live"))]));
                    show(&entries, cx);
                }
                Step::WheelUp(y) | Step::WheelDown(y) => {
                    let y = if matches!(step, Step::WheelUp(_)) { y } else { -y };
                    cx.simulate_event(gpui::ScrollEvent {
                        position: middle,
                        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(y))),
                        ..Default::default()
                    });
                    show(&entries, cx);
                }
                Step::Press => {
                    cx.simulate_event(gpui::MouseDownEvent {
                        button: MouseButton::Left,
                        position: middle,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    });
                    history.push(format!("{round} pressed -> {:?}", probe(cx)));
                    cx.simulate_event(gpui::MouseUpEvent {
                        button: MouseButton::Left,
                        position: middle,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                    });
                    history.push(format!("{round} released -> {:?}", probe(cx)));
                    show(&entries, cx);
                }
            }
            let after = probe(cx);
            history.push(format!("{round} {step:?} -> {after:?}"));
            let said = format!(
                "round {round}, {step:?}: {before:?} -> {after:?}; before it: {}",
                history.iter().rev().skip(1).take(8).rev().cloned().collect::<Vec<_>>().join("; ")
            );
            // Let go, the view is held by a row, never left to the layout's hold on the end,
            // which would carry it along with what lands below.
            if !after.0 {
                assert_ne!(after.2.as_ref().map(|(key, _)| key.as_ref()), Some(END), "let go, a row holds it: {said}");
            }
            match step {
                Step::Below(_) | Step::Above(_) | Step::Stream(_) => {
                    assert_eq!(after.0, before.0, "an arrival never takes or gives the end: {said}");
                    if before.0 {
                        assert!(after.1 <= FOLLOW_PX, "following, the view stays at the end: {said}");
                    } else {
                        let (was, y0) = before.2.clone().unwrap_or_else(|| panic!("a row in view before: {said}"));
                        let (now, y1) = after.2.clone().unwrap_or_else(|| panic!("a row in view after: {said}"));
                        assert_eq!(was, now, "the first row in view keeps its key: {said}");
                        assert!((y0 - y1).abs() < 0.5, "and its place: {said}");
                    }
                }
                Step::WheelUp(_) => {
                    assert_eq!(after.0, after.1 <= FOLLOW_PX, "up the list, it lets go: {said}");
                }
                Step::WheelDown(_) => {
                    if after.1 <= FOLLOW_PX {
                        assert!(after.0, "back at the end, it follows: {said}");
                    }
                }
                Step::Press => assert!(!after.0, "a press in a row lets go: {said}"),
            }
            if after.0 {
                assert_eq!(transcript.read_with(cx, |this, _| this.jump_unseen()), 0, "{said}");
            }
        }
        // Back to the end through the pill, whatever stands below.
        transcript.update(cx, |this, cx| this.jump_to_bottom(cx));
        show(&entries, cx);
        let end = probe(cx);
        assert!(end.0 && end.1 <= FOLLOW_PX, "the jump follows the end again: {end:?}");
    }

    #[gpui::test]
    fn departing_transcript_is_retained_only_until_hidden(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            let state = cx.new(|_| AppState::new());
            let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
            transcript.update(cx, |transcript, cx| {
                transcript.retain_for_route_exit();
                transcript.chat_id = Some("departing".into());
                transcript.last_source = None;
                transcript.rows = vec![viewport_row("row", "message")];
                transcript.list.reset(1);
                transcript.sync(cx);
                assert_eq!(transcript.rows.len(), 1);
                assert!(transcript.route_exit_pending(cx));
                transcript.finish_route_exit(cx);
                assert!(transcript.rows.is_empty());
                assert!(transcript.chat_id.is_none());
                assert!(!transcript.route_exit_pending(cx));
            });
        });
    }
    use slopty_view_doc::MessagePart;

    fn with_tool_group_navigation(
        cx: &mut gpui::TestAppContext,
        run: impl FnOnce(Entity<AppState>, Entity<Transcript>, &mut gpui::App),
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            let state = cx.new(|_| AppState::new());
            let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
            transcript.update(cx, |this, _| this.retain_for_route_exit());
            replay_tool_group(&state, &transcript, "chat-a", cx);
            run(state, transcript, cx);
        });
    }

    fn replay_tool_group(
        state: &Entity<AppState>,
        transcript: &Entity<Transcript>,
        chat: &str,
        cx: &mut gpui::App,
    ) {
        state.update(cx, |state, _| {
            state.selected_chat = Some(chat.into());
            state.transcript_replayed = true;
            state.transcript = vec![assistant(
                "tools",
                MessageStatus::Complete,
                // slopty: two calls: a lone one shows itself, with nothing to fold (#2).
                vec![tool_part("call", "pwd"), tool_part("call-2", "cargo check")],
            )];
            state.transcript_revision += 1;
        });
        transcript.update(cx, |this, cx| this.sync(cx));
    }

    fn assert_replayed_group_is_closed(transcript: &Entity<Transcript>, cx: &mut gpui::App) {
        transcript.update(cx, |this, cx| {
            let row = this
                .rows
                .iter()
                .find(|row| matches!(row.kind, RowKind::ToolGroup { .. }))
                .unwrap()
                .clone();
            let RowKind::ToolGroup { tools, summary } = &row.kind else {
                unreachable!()
            };
            let _ = this.render_tool_group(&row.id, tools, summary, &Theme::dark(), cx);
            let reveal = &this.tool_group_reveals[&row.id];
            assert_eq!(
                reveal.rendered_open,
                Some(false),
                "history flashed open on its first render"
            );
            assert_eq!(
                reveal.rendered_height, 0.0,
                "history replayed a stale closing tween"
            );
            assert!(reveal.header_started_at.is_none());
            assert!(reveal.starts.iter().all(Option::is_none));
        });
    }

    #[gpui::test]
    fn tool_groups_stay_closed_on_populated_chat_attach(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            for chat in ["chat-b", "chat-a", "chat-b"] {
                // Selection and cached replay can coalesce into a single sync.
                replay_tool_group(&state, &transcript, chat, cx);
                assert_replayed_group_is_closed(&transcript, cx);
            }
        });
    }

    #[gpui::test]
    fn tool_groups_stay_closed_after_rapid_new_chat_navigation(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            for finish_exit in [false, true] {
                transcript.update(cx, |this, _| {
                    // Navigate away during the previous close animation.
                    this.folds.insert(
                        "tools#g:call".into(),
                        FoldState {
                            open: Some(false),
                            from: 120.0,
                            toggled_at: Some(Instant::now()),
                            disclosure_at: Some(Instant::now()),
                            ..Default::default()
                        },
                    );
                });
                state.update(cx, |state, cx| state.select_chat(None, cx));
                transcript.update(cx, |this, cx| {
                    this.sync(cx);
                    if finish_exit {
                        this.finish_route_exit(cx);
                    }
                });
                state.update(cx, |state, cx| state.select_chat(Some("chat-a".into()), cx));
                transcript.update(cx, |this, cx| this.sync(cx));
                replay_tool_group(&state, &transcript, "chat-a", cx);
                assert_replayed_group_is_closed(&transcript, cx);
            }
        });
    }

    /// slopty: a turn's work the person opened is open again when they come back to its chat,
    /// a subagent drilled into and left, so the saved viewport lands on rows still drawn; one they
    /// never touched comes back shut, and no strip comes back lit by the pointer that left.
    #[gpui::test]
    fn a_chat_come_back_to_keeps_the_work_its_person_opened(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            let work: SharedString = "tools#g:call".into();
            let open = |transcript: &Entity<Transcript>, cx: &mut gpui::App| {
                transcript.read(cx).folds.get(&work).and_then(|fold| fold.open) == Some(true)
            };
            assert!(!open(&transcript, cx), "the work starts shut");
            transcript.update(cx, |this, cx| this.toggle_fold(work.clone(), 0.0, cx));
            assert!(open(&transcript, cx), "a click opens the work");

            for away in ["chat-b", "chat-c"] {
                // The pointer on the reply as it leaves: the row unmounts with no leave.
                transcript.update(cx, |this, _| {
                    this.hovered_entry = Some((work.clone(), "tools".into()));
                });
                state.update(cx, |state, cx| state.select_chat(Some(away.into()), cx));
                transcript.update(cx, |this, cx| this.sync(cx));
                replay_tool_group(&state, &transcript, away, cx);
                assert!(!open(&transcript, cx), "{away}'s own work stays shut");
                replay_tool_group(&state, &transcript, "chat-a", cx);
                assert!(open(&transcript, cx), "back from {away}, the work is open as it was left");
                assert!(
                    transcript.read(cx).hovered_entry.is_none(),
                    "back from {away}, no strip is lit by a pointer that left with the chat",
                );
            }
        });
    }

    #[gpui::test]
    fn tool_group_revisit_skips_batched_history_but_animates_live_arrivals(
        cx: &mut gpui::TestAppContext,
    ) {
        use slopty_view_doc::transcript_delta::{TranscriptFrame, diff_transcript};

        with_tool_group_navigation(cx, |state, transcript, cx| {
            let apply_frame = |frame, replay_baseline, cx: &mut gpui::App| {
                state.update(cx, |state, cx| {
                    state
                        .receive_transcript_update(
                            slopty_view_doc::TranscriptUpdate {
                                frame,
                                replay_baseline,
                            },
                            cx,
                        )
                        .unwrap();
                });
                transcript.update(cx, |this, cx| this.sync(cx));
            };
            let cached = vec![assistant(
                "tools",
                MessageStatus::Streaming,
                vec![tool_part("call", "pwd")],
            )];
            apply_frame(
                TranscriptFrame::reset(&cached),
                Some(slopty_view_doc::TranscriptBaseline::capture(&cached)),
                cx,
            );
            state.update(cx, |state, cx| state.select_chat(Some("chat-b".into()), cx));
            transcript.update(cx, |this, cx| this.sync(cx));

            // The agent does this work while A is away. On return, a stale
            // local snapshot can arrive before remote catch-up deltas. Keep
            // the same streaming message/group throughout: its status alone
            // cannot distinguish historical tools from new live arrivals.
            let mut first_batch = cached.clone();
            first_batch[0].parts.extend([
                tool_part("away-1", "ls"),
                tool_part("away-2", "git status --short"),
            ]);
            let mut second_batch = first_batch.clone();
            second_batch[0].parts.extend([
                tool_part("away-3", "git diff --stat"),
                tool_part("away-4", "git log -1"),
            ]);

            state.update(cx, |state, cx| state.select_chat(Some("chat-a".into()), cx));
            transcript.update(cx, |this, cx| this.sync(cx));
            assert!(
                !transcript.read(cx).rows.is_empty(),
                "revisited transcript must have visible rows before the new watch responds"
            );
            apply_frame(
                TranscriptFrame::reset(&cached),
                Some(slopty_view_doc::TranscriptBaseline::capture(&cached)),
                cx,
            );
            let row_id: SharedString = "tools#g:call".into();
            {
                let reveal = &transcript.read(cx).tool_group_reveals[&row_id];
                assert_eq!(reveal.starts.len(), 1);
                assert!(reveal.header_started_at.is_none());
                assert!(reveal.starts.iter().all(Option::is_none));
            }

            // Drive the production update reducer with the historical cutoff
            // supplied by the engine on each backfill batch. Inspect epochs
            // without sleeping or advancing
            // frames, so slow test machines cannot hide a replayed entrance.
            let mut historical_entrances = Vec::new();
            for (previous, next) in [(&cached, &first_batch), (&first_batch, &second_batch)] {
                let frame = diff_transcript(previous, next);
                assert!(matches!(&frame, TranscriptFrame::Delta { .. }));
                apply_frame(
                    frame,
                    Some(slopty_view_doc::TranscriptBaseline::capture(next)),
                    cx,
                );
                let reveal = &transcript.read(cx).tool_group_reveals[&row_id];
                assert_eq!(reveal.starts.len(), next[0].parts.len());
                assert!(reveal.header_started_at.is_none());
                historical_entrances.push(reveal.starts.iter().flatten().count());
            }

            // Once caught up, a genuinely new tool in that same group still
            // needs its entrance. Check this before reporting accumulated
            // history failures, so both halves of the contract are exercised.
            let mut live = second_batch.clone();
            live[0].parts.push(tool_part("live-call", "git diff"));
            apply_frame(diff_transcript(&second_batch, &live), None, cx);
            let reveal = &transcript.read(cx).tool_group_reveals[&row_id];
            assert_eq!(reveal.starts.len(), 6);
            assert!(
                reveal.starts[5].is_some(),
                "a new live tool must retain its entrance after catch-up"
            );
            assert!(reveal.header_started_at.is_none());
            assert_eq!(
                historical_entrances,
                vec![0, 0],
                "tools accumulated while away must not acquire entrance animations in either catch-up batch"
            );
        });
    }

    #[test]
    fn background_preparation_reuses_unchanged_rows_and_replaces_same_length_text() {
        let mut worker = TranscriptPreparation::default();
        let original = vec![
            assistant(
                "a",
                MessageStatus::Complete,
                vec![MessagePart::Text {
                    id: "p".into(),
                    text: "alpha".into(),
                }],
            ),
            assistant("b", MessageStatus::Complete, vec![tool_part("t", "pwd")]),
        ];
        let first = worker
            .prepare(&slopty_view_doc::TranscriptUpdate {
                frame: slopty_view_doc::TranscriptFrame::reset(&original),
                replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(&original)),
            })
            .unwrap();
        let mut changed = original.clone();
        changed[0].parts = vec![MessagePart::Text {
            id: "p".into(),
            text: "omega".into(),
        }];
        let next = worker
            .prepare(&slopty_view_doc::TranscriptUpdate {
                frame: slopty_view_doc::diff_transcript(&original, &changed),
                replay_baseline: None,
            })
            .unwrap();
        assert!(Arc::ptr_eq(&first.rows["b"], &next.rows["b"]));
        assert!(!Arc::ptr_eq(&first.rows["a"], &next.rows["a"]));
        assert!(diff_rows(&first.rows["a"], &next.rows["a"]).is_some());
    }

    #[gpui::test]
    fn prepared_whale_open_and_revisit_do_not_build_rows_on_ui(cx: &mut gpui::TestAppContext) {
        let (update, prepared, preparation_ms) = std::thread::spawn(|| {
            // slopty: replaying a `SLOPTY_WHALE_SNAPSHOT` needs the cut loro document.
            let entries = {
                vec![assistant("whale-turn", MessageStatus::Complete, (0..5000).map(|i| {
                    MessagePart::Text { id: format!("part-{i}"), text: format!("## Result {i}\n\n**Markdown** with `code` and [links](https://example.com).\n") }
                }).collect())]
            };
            let update = slopty_view_doc::TranscriptUpdate {
                replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(&entries)),
                frame: slopty_view_doc::TranscriptFrame::Reset { reset: entries },
            };
            let start = Instant::now();
            let prepared = TranscriptPreparation::default().prepare(&update).unwrap();
            (update, prepared, start.elapsed().as_millis())
        }).join().unwrap();
        with_tool_group_navigation(cx, |state, transcript, cx| {
            state.update(cx, |state, cx| state.select_chat(Some("whale".into()), cx));
            transcript.update(cx, |this, cx| this.sync(cx));
            FORBID_ROW_PREPARATION.with(|flag| flag.set(true));
            let start = Instant::now();
            state.update(cx, |state, cx| {
                state
                    .receive_opening_transcript_update(update, false, cx)
                    .unwrap();
                state
                    .prepared_transcripts
                    .insert("whale".into(), prepared.clone());
            });
            transcript.update(cx, |this, cx| this.sync(cx));
            let open_ms = start.elapsed().as_millis();
            assert!(!transcript.read(cx).rows.is_empty());
            assert!(
                transcript
                    .read(cx)
                    .tool_group_reveals
                    .values()
                    .all(|r| r.starts.iter().all(Option::is_none))
            );
            let start = Instant::now();
            state.update(cx, |state, cx| state.select_chat(None, cx));
            transcript.update(cx, |this, cx| this.sync(cx));
            state.update(cx, |state, cx| state.select_chat(Some("whale".into()), cx));
            transcript.update(cx, |this, cx| this.sync(cx));
            let revisit_ms = start.elapsed().as_millis();
            assert!(Arc::ptr_eq(
                &state.read(cx).prepared_transcripts["whale"],
                &prepared
            ));
            assert!(
                transcript
                    .read(cx)
                    .tool_group_reveals
                    .values()
                    .all(|r| r.starts.iter().all(Option::is_none))
            );
            FORBID_ROW_PREPARATION.with(|flag| flag.set(false));
            eprintln!(
                "background_preparation_ms={preparation_ms} ui_open_ms={open_ms} ui_revisit_ms={revisit_ms}"
            );
        });
    }

    #[gpui::test]
    fn opening_tail_full_history_and_cached_revisit_never_replay_tool_entrances(
        cx: &mut gpui::TestAppContext,
    ) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            state.update(cx, |state, cx| state.select_chat(Some("whale".into()), cx));
            transcript.update(cx, |this, cx| this.sync(cx));
            let preview = vec![assistant(
                "turn",
                MessageStatus::Streaming,
                vec![tool_part("tail-tool", "pwd")],
            )];
            let full = vec![assistant(
                "turn",
                MessageStatus::Streaming,
                vec![
                    tool_part("older-tool", "ls"),
                    MessagePart::Text {
                        id: "separator".into(),
                        text: "Earlier result".into(),
                    },
                    tool_part("tail-tool", "pwd"),
                ],
            )];
            let apply = |entries: &[SessionMessageEntry], pending, cx: &mut gpui::App| {
                state.update(cx, |state, cx| {
                    state
                        .receive_opening_transcript_update(
                            slopty_view_doc::TranscriptUpdate {
                                frame: slopty_view_doc::TranscriptFrame::reset(entries),
                                replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(
                                    entries,
                                )),
                            },
                            pending,
                            cx,
                        )
                        .unwrap();
                });
                transcript.update(cx, |this, cx| this.sync(cx));
            };
            let assert_settled = |expected_tools, cx: &mut gpui::App| {
                let this = transcript.read(cx);
                assert_eq!(
                    this.tool_group_reveals
                        .values()
                        .map(|r| r.starts.len())
                        .sum::<usize>(),
                    expected_tools
                );
                for reveal in this.tool_group_reveals.values() {
                    assert!(
                        reveal.header_started_at.is_none(),
                        "historical header replayed"
                    );
                    assert!(
                        reveal.starts.iter().all(Option::is_none),
                        "historical tool replayed"
                    );
                }
            };
            apply(&preview, true, cx);
            assert_settled(1, cx);
            // Prepending history puts the older call, then the narration after it, ahead of the
            // tail tool: two groups in record order, the words between them. The tail tool's
            // new place must not turn it into a live entrance.
            apply(&full, false, cx);
            assert_settled(2, cx);
            for _ in 0..3 {
                state.update(cx, |state, cx| state.select_chat(Some("away".into()), cx));
                transcript.update(cx, |this, cx| this.sync(cx));
                state.update(cx, |state, cx| state.select_chat(Some("whale".into()), cx));
                transcript.update(cx, |this, cx| this.sync(cx));
                assert_settled(2, cx);
                apply(&preview, true, cx); // ignored because the cache is complete
                assert_settled(2, cx);
                apply(&full, false, cx);
                assert_settled(2, cx);
            }
            let mut live = full.clone();
            live[0].parts.push(tool_part("new-live-tool", "git status"));
            state.update(cx, |state, cx| {
                state
                    .receive_transcript_update(
                        slopty_view_doc::TranscriptUpdate {
                            frame: slopty_view_doc::diff_transcript(&full, &live),
                            replay_baseline: None,
                        },
                        cx,
                    )
                    .unwrap();
            });
            transcript.update(cx, |this, cx| this.sync(cx));
            let this = transcript.read(cx);
            let older = &this.tool_group_reveals[&SharedString::from("turn#g:older-tool")];
            assert!(older.header_started_at.is_none());
            assert!(older.starts.iter().all(Option::is_none), "existing work must remain settled");
            let work = &this.tool_group_reveals[&SharedString::from("turn#g:tail-tool")];
            assert!(work.starts[0].is_none(), "existing work must remain settled");
            assert!(work.starts[1].is_some(), "new live tool must still animate");
        });
    }

    #[gpui::test]
    fn tool_group_replay_cutoff_survives_coalesced_live_updates(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            let history = vec![assistant(
                "tools",
                MessageStatus::Streaming,
                vec![tool_part("call", "pwd"), tool_part("away", "ls")],
            )];
            let mut live = history.clone();
            live[0].parts.push(tool_part("live", "git diff"));
            live.push(assistant(
                "new-live-group",
                MessageStatus::Streaming,
                vec![tool_part("new", "git status")],
            ));
            state.update(cx, |state, cx| {
                let frame = slopty_view_doc::diff_transcript(&state.transcript, &history);
                state
                    .receive_transcript_update(
                        slopty_view_doc::TranscriptUpdate {
                            frame,
                            replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(&history)),
                        },
                        cx,
                    )
                    .unwrap();
                // Both updates land before the transcript observes/render them.
                state
                    .receive_transcript_update(
                        slopty_view_doc::TranscriptUpdate {
                            frame: slopty_view_doc::diff_transcript(&history, &live),
                            replay_baseline: None,
                        },
                        cx,
                    )
                    .unwrap();
            });
            transcript.update(cx, |this, cx| {
                this.sync(cx);
                let reveal = &this.tool_group_reveals[&SharedString::from("tools#g:call")];
                assert!(reveal.header_started_at.is_none());
                assert!(reveal.starts[..2].iter().all(Option::is_none));
                assert!(
                    reveal.starts[2].is_some(),
                    "live tail was swallowed by the replay"
                );
                let new_group = &this.tool_group_reveals[&SharedString::from("new-live-group#g:new")];
                assert!(new_group.header_started_at.is_some());
                assert!(new_group.starts[0].is_some());
            });
        });
    }

    #[gpui::test]
    fn tool_group_interleaved_history_preserves_live_animation_epochs(
        cx: &mut gpui::TestAppContext,
    ) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            let apply = |entries: &[SessionMessageEntry], baseline, cx: &mut gpui::App| {
                state.update(cx, |state, cx| {
                    let frame = slopty_view_doc::diff_transcript(&state.transcript, entries);
                    state
                        .receive_transcript_update(
                            slopty_view_doc::TranscriptUpdate {
                                frame,
                                replay_baseline: baseline,
                            },
                            cx,
                        )
                        .unwrap();
                });
                transcript.update(cx, |this, cx| this.sync(cx));
            };
            // Two live tools are already animating when older work arrives
            // before and between them in the same group.
            let mut entries = vec![assistant(
                "mixed",
                MessageStatus::Streaming,
                vec![tool_part("live-a", "pwd"), tool_part("live-b", "pwd")],
            )];
            apply(&entries, None, cx);
            let row: SharedString = "mixed#g:live-a".into();
            let starts = transcript.read(cx).tool_group_reveals[&row].starts.clone();
            assert!(starts.iter().all(Option::is_some));
            let header = transcript.read(cx).tool_group_reveals[&row].header_started_at;
            entries[0].parts.insert(1, tool_part("old-a", "pwd"));
            entries[0].parts.insert(3, tool_part("old-b", "pwd"));
            let mut historical = entries.clone();
            historical[0].parts.retain(|p| p.id().starts_with("old"));
            apply(
                &entries,
                Some(slopty_view_doc::TranscriptBaseline::capture(&historical)),
                cx,
            );
            let reveal = &transcript.read(cx).tool_group_reveals[&row];
            assert_eq!(reveal.starts, vec![starts[0], None, starts[1], None]);
            assert_eq!(reveal.header_started_at, header);
            // A further mixed frame must animate only its new live activity.
            entries[0].parts.push(tool_part("old-c", "pwd"));
            historical[0].parts.push(tool_part("old-c", "pwd"));
            entries[0].parts.push(tool_part("live-c", "pwd"));
            apply(
                &entries,
                Some(slopty_view_doc::TranscriptBaseline::capture(&historical)),
                cx,
            );
            let reveal = &transcript.read(cx).tool_group_reveals[&row];
            assert_eq!(reveal.starts[..5], [starts[0], None, starts[1], None, None]);
            assert!(reveal.starts[5].is_some());
        });
    }

    #[gpui::test]
    fn tool_group_first_live_arrival_after_empty_replay_animates(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            state.update(cx, |state, cx| {
                state.select_chat(Some("new-chat".into()), cx);
                state
                    .receive_transcript_update(
                        slopty_view_doc::TranscriptUpdate {
                            frame: slopty_view_doc::TranscriptFrame::reset(&[]),
                            replay_baseline: Some(Default::default()),
                        },
                        cx,
                    )
                    .unwrap();
            });
            transcript.update(cx, |this, cx| this.sync(cx));
            let live = vec![assistant(
                "first",
                MessageStatus::Streaming,
                vec![tool_part("first-call", "pwd")],
            )];
            state.update(cx, |state, cx| {
                state
                    .receive_transcript_update(
                        slopty_view_doc::TranscriptUpdate {
                            frame: slopty_view_doc::diff_transcript(&[], &live),
                            replay_baseline: None,
                        },
                        cx,
                    )
                    .unwrap();
            });
            transcript.update(cx, |this, cx| {
                this.sync(cx);
                let reveal = &this.tool_group_reveals[&SharedString::from("first#g:first-call")];
                assert!(reveal.header_started_at.is_some());
                assert!(reveal.starts[0].is_some());
            });
        });
    }

    #[gpui::test]
    fn tool_group_live_reset_does_not_become_history(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            let mut live = state.read(cx).transcript.clone();
            for ix in 0..5 {
                live.push(assistant(
                    &format!("live-{ix}"),
                    MessageStatus::Streaming,
                    vec![tool_part("call", "pwd")],
                ));
            }
            state.update(cx, |state, cx| {
                let frame = slopty_view_doc::diff_transcript(&state.transcript, &live);
                assert!(matches!(&frame, slopty_view_doc::TranscriptFrame::Reset { .. }));
                state
                    .receive_transcript_update(
                        slopty_view_doc::TranscriptUpdate {
                            frame,
                            replay_baseline: None,
                        },
                        cx,
                    )
                    .unwrap();
            });
            transcript.update(cx, |this, cx| {
                this.sync(cx);
                for ix in 0..5 {
                    let reveal =
                        &this.tool_group_reveals[&SharedString::from(format!("live-{ix}#g:call"))];
                    assert!(reveal.header_started_at.is_some());
                    assert!(reveal.starts[0].is_some());
                }
            });
        });
    }

    #[gpui::test]
    fn tool_group_navigation_keeps_user_pins_and_new_arrivals(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            transcript.update(cx, |this, _| {
                this.folds.insert(
                    "tools#g:call".into(),
                    FoldState {
                        open: Some(true),
                        ..Default::default()
                    },
                );
            });
            state.update(cx, |state, cx| state.select_chat(None, cx));
            transcript.update(cx, |this, cx| this.sync(cx));
            // Cached replay can land without an intervening pending frame.
            replay_tool_group(&state, &transcript, "chat-a", cx);
            transcript.update(cx, |this, cx| {
                let row = this.rows[0].clone();
                let RowKind::ToolGroup { tools, summary } = &row.kind else {
                    panic!("expected tools")
                };
                let _ = this.render_tool_group(&row.id, tools, summary, &Theme::dark(), cx);
                assert_eq!(this.folds[&row.id].open, Some(true));
                assert_eq!(this.tool_group_reveals[&row.id].rendered_open, Some(true));
                assert!(
                    this.tool_group_reveals[&row.id]
                        .starts
                        .iter()
                        .all(Option::is_none)
                );
            });
            state.update(cx, |state, _| {
                state.transcript.push(assistant(
                    "live-tools",
                    MessageStatus::Streaming,
                    vec![tool_part("new-call", "ls")],
                ));
                state.transcript_revision += 1;
            });
            transcript.update(cx, |this, cx| {
                this.sync(cx);
                let row = this.rows.last().unwrap().clone();
                let RowKind::ToolGroup { tools, summary } = &row.kind else {
                    panic!("expected tools")
                };
                let _ = this.render_tool_group(&row.id, tools, summary, &Theme::dark(), cx);
                let reveal = &this.tool_group_reveals[&row.id];
                assert!(reveal.header_started_at.is_some());
                assert!(reveal.starts.iter().all(Option::is_some));
                assert_eq!(reveal.rendered_open, Some(true));
            });
        });
    }

    #[test]
    fn resizing_details_does_not_restart_group_disclosure() {
        let now = Instant::now();
        let fold = FoldState {
            toggled_at: Some(now),
            ..Default::default()
        };
        assert_eq!(tool_disclosure_progress(true, fold, now), 1.0);
        assert_eq!(tool_disclosure_progress(false, fold, now), 0.0);
    }

    #[test]
    fn connector_intersection_is_tessellated_only_once() {
        let mut path = PathBuilder::fill().with_style(gpui::PathStyle::Fill(
            gpui::FillOptions::default().with_fill_rule(gpui::FillRule::NonZero),
        ));
        activity_ribbon(
            &mut path,
            &[point(px(0.0), px(0.0)), point(px(0.0), px(10.0))],
        );
        activity_ribbon(
            &mut path,
            &[point(px(-5.0), px(5.0)), point(px(5.0), px(5.0))],
        );
        let path = path.build().unwrap();
        let area: f32 = path
            .vertices
            .chunks_exact(3)
            .map(|triangle| {
                let a = triangle[0].xy_position;
                let b = triangle[1].xy_position;
                let c = triangle[2].xy_position;
                (f32::from(b.x - a.x) * f32::from(c.y - a.y)
                    - f32::from(b.y - a.y) * f32::from(c.x - a.x))
                .abs()
                    / 2.0
            })
            .sum();
        assert!(
            (area - 19.0).abs() < 0.001,
            "overlap must contribute once: {area}"
        );
    }

    #[test]
    fn file_badge_icon_well_counter_shades_each_appearance() {
        for (mut theme, base) in [
            (Theme::dark(), crate::theme::grey(48)),
            (Theme::light(), crate::theme::grey(230)),
        ] {
            theme.surface_treatment = slopty_shell_theme::SurfaceTreatment::Opaque;
            let badge = crate::theme::flatten(theme.ink(0.06), base);
            let icon_well = crate::theme::flatten(crate::file_icons::well_bg(&theme), badge);
            let contrast = crate::theme::contrast_ratio(icon_well, badge);

            match theme.appearance {
                crate::theme::Appearance::Dark => assert!(icon_well.l < badge.l),
                crate::theme::Appearance::Light => assert!(icon_well.l > badge.l),
            }
            assert!(contrast > 1.04, "icon well must remain visible: {contrast}");
            assert!(
                contrast < 1.20,
                "icon well must not become a harsh split surface: {contrast}"
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_badge_icon_well_uses_more_coverage_on_frost() {
        for mut theme in [Theme::dark(), Theme::light()] {
            theme.surface_treatment = slopty_shell_theme::SurfaceTreatment::Opaque;
            let opaque_alpha = crate::file_icons::well_bg(&theme).a;
            theme.surface_treatment = slopty_shell_theme::SurfaceTreatment::Frosted;
            let frosted = crate::file_icons::well_bg(&theme);
            let badge = crate::theme::flatten(theme.ink(0.06), theme.bg);
            let icon_well = crate::theme::flatten(frosted, badge);
            let contrast = crate::theme::contrast_ratio(icon_well, badge);

            assert!(opaque_alpha > 0.0 && opaque_alpha < 1.0);
            assert!(frosted.a > opaque_alpha && frosted.a < 1.0);
            assert!(
                contrast > 1.03,
                "frosted icon well must remain visible: {contrast}"
            );
            assert!(
                contrast < 1.20,
                "frosted icon well must not become a harsh split surface: {contrast}"
            );
        }
    }

    #[test]
    fn selection_scroll_ramps_at_viewport_edges() {
        let bounds = Bounds::new(
            gpui::point(px(10.0), px(20.0)),
            gpui::size(px(300.0), px(200.0)),
        );
        assert_eq!(
            selection_scroll_step(bounds, gpui::point(px(20.0), px(120.0))),
            0.0
        );
        assert!(selection_scroll_step(bounds, gpui::point(px(20.0), px(20.0))) < 0.0);
        assert!(selection_scroll_step(bounds, gpui::point(px(20.0), px(220.0))) > 0.0);
        assert!(
            selection_scroll_step(bounds, gpui::point(px(20.0), px(220.0)))
                > selection_scroll_step(bounds, gpui::point(px(20.0), px(200.0)))
        );
    }

    // ---- streaming parse wiring (the transcript side, not the parser) ----

    #[test]
    fn live_row_parse_work_is_bounded_per_commit() {
        // Drive the EXACT wiring `rows_for` uses (`parse_for_row`) with the
        // prefix-extending commit snapshots the doc watch delivers, and prove
        // the per-commit parse work stays O(reparsed tail): a full-reparse
        // wiring would feed ~N/2 × final_len bytes through the parser across N
        // commits; the incremental path stays within a small multiple of the
        // final length regardless of N.
        let mut live_parsers = HashMap::new();
        let mut tree_cache = HashMap::new();
        let paragraph = "A paragraph of streaming prose that keeps arriving.\n\n";
        let commits = 120usize;
        let mut text = String::new();
        let mut total_parsed = 0usize;
        for i in 0..commits {
            // Each commit appends ~half a paragraph (crosses block boundaries).
            let chunk = &paragraph[..paragraph.len() / 2];
            text.push_str(if i % 2 == 0 {
                chunk
            } else {
                &paragraph[paragraph.len() / 2..]
            });
            let (tree, outcome) =
                parse_for_row(true, "e1#p1", &text, &mut live_parsers, &mut tree_cache);
            assert!(!tree.blocks.is_empty());
            let ParseOutcome::Incremental {
                parsed_bytes,
                stable_prefix_blocks,
            } = outcome
            else {
                panic!("streaming commit must take the incremental path");
            };
            total_parsed += parsed_bytes;
            // Per commit: never a full reparse once the doc has grown past the
            // tail window (last two complete blocks + the partial trailing
            // one + the delta ≤ 3 paragraphs here).
            assert!(
                parsed_bytes <= 3 * paragraph.len(),
                "commit {i}: parsed {parsed_bytes} bytes — not bounded by the tail window"
            );
            // The stable prefix grows with the doc — settled blocks are never
            // re-touched (this is what keeps render caches valid).
            assert!(stable_prefix_blocks + 2 >= tree.blocks.len().saturating_sub(1));
        }
        // Across the whole stream: work is commits × O(tail), an order of
        // magnitude under the ~commits × len/2 a full-reparse wiring costs.
        let final_len = text.len();
        let full_reparse_cost = commits * final_len / 2;
        assert!(total_parsed <= commits * 3 * paragraph.len());
        assert!(
            total_parsed * 10 < full_reparse_cost,
            "total parsed {total_parsed} vs full-reparse ~{full_reparse_cost}"
        );

        // Live→complete handoff: the completed part adopts the live parser's
        // exact tree without parsing a single byte.
        let (_, outcome) = parse_for_row(false, "e1#p1", &text, &mut live_parsers, &mut tree_cache);
        assert_eq!(outcome, ParseOutcome::Handoff);
        // And the settled cache serves repeats with no work at all.
        let (_, outcome) = parse_for_row(false, "e1#p1", &text, &mut live_parsers, &mut tree_cache);
        assert_eq!(outcome, ParseOutcome::Cached);
    }











    fn viewport_row(id: &str, entry_id: &str) -> Row {
        Row {
            id: id.into(),
            version: 0,
            turn_start: true,
            kind: RowKind::ErrorChip {
                message: SharedString::default(),
            },
            entry_id: entry_id.into(),
            timestamp: None,
            copy_text: None,
            compact_fold: None,
        }
    }

    #[test]
    fn viewport_anchor_tracks_a_stable_row_across_replay() {
        let rows = vec![
            viewport_row("a", "entry-a"),
            viewport_row("b", "entry-b"),
            viewport_row("c", "entry-c"),
        ];
        let anchor = ViewportAnchor::capture(
            &rows,
            ListOffset {
                item_ix: 1,
                offset_in_item: px(23.0),
            },
        )
        .expect("visible row");

        let replay = vec![
            viewport_row("new", "entry-new"),
            viewport_row("a", "entry-a"),
            viewport_row("b", "entry-b"),
            viewport_row("c", "entry-c"),
        ];
        let restored = anchor.resolve(&replay).expect("restored row");
        assert_eq!(restored.item_ix, 2);
        assert_eq!(restored.offset_in_item, px(23.0));
    }

    #[test]
    fn viewport_anchor_has_entry_and_index_fallbacks() {
        let rows = vec![
            viewport_row("a", "entry-a"),
            viewport_row("b", "entry-b"),
            viewport_row("old-block", "entry-c"),
        ];
        let anchor = ViewportAnchor::capture(
            &rows,
            ListOffset {
                item_ix: 2,
                offset_in_item: px(31.0),
            },
        )
        .expect("visible row");

        let reshaped = vec![
            viewport_row("a", "entry-a"),
            viewport_row("b", "entry-b"),
            viewport_row("inserted", "entry-new"),
            viewport_row("new-block", "entry-c"),
        ];
        let same_entry = anchor.resolve(&reshaped).expect("entry fallback");
        assert_eq!(same_entry.item_ix, 3);
        assert_eq!(same_entry.offset_in_item, px(0.0));

        let entry_removed = vec![viewport_row("a", "entry-a"), viewport_row("b", "entry-b")];
        let clamped = anchor.resolve(&entry_removed).expect("index fallback");
        assert_eq!(clamped.item_ix, 1);
        assert_eq!(clamped.offset_in_item, px(0.0));
    }

    #[test]
    fn optimistic_echo_cannot_consume_a_historical_viewport_before_replay() {
        let history = vec![viewport_row("historical", "historical-entry")];
        let saved = SavedViewport::capture(&history, ListOffset::default(), false, 480.0)
            .expect("historical viewport");
        let echo_only = vec![viewport_row("echo", "echo-entry")];

        assert!(
            saved.resolve(&echo_only, false).is_none(),
            "an unrelated echo is not an authoritative index fallback"
        );
        assert_eq!(
            saved
                .resolve(&echo_only, true)
                .expect("populated replay may use an index fallback")
                .offset
                .item_ix,
            0
        );
        assert!(TranscriptReplayState::Empty.authoritative_empty());
        assert!(!TranscriptReplayState::Empty.allows_fallback());
        assert!(!TranscriptReplayState::Pending.allows_fallback());
        assert!(TranscriptReplayState::Populated.allows_fallback());

        let echo_viewport =
            SavedViewport::capture(&echo_only, ListOffset::default(), false, 0.0)
                .expect("echo viewport");
        assert!(
            echo_viewport.resolve(&echo_only, false).is_some(),
            "the exact optimistic row is safe before replay"
        );
    }




    #[test]
    fn pinned_viewports_follow_tail_and_the_cache_is_bounded() {
        let rows = vec![viewport_row("row", "entry")];
        let pinned = SavedViewport::capture(&rows, ListOffset::default(), true, 999.0)
            .expect("pinned viewport");
        assert!(matches!(pinned, SavedViewport::FollowTail));

        let mut cache = SavedViewportCache::default();
        for ix in 0..MAX_SAVED_VIEWPORTS + 8 {
            cache.insert(format!("chat-{ix}"), SavedViewport::FollowTail);
        }
        assert_eq!(cache.len(), MAX_SAVED_VIEWPORTS);
        assert!(cache.get_cloned_and_touch("chat-0").is_none());
        assert!(
            cache
                .get_cloned_and_touch(&format!("chat-{}", MAX_SAVED_VIEWPORTS + 7))
                .is_some()
        );
    }

    #[test]
    fn reopening_the_oldest_cached_chat_protects_it_from_the_next_eviction() {
        let mut cache = SavedViewportCache::default();
        for ix in 0..MAX_SAVED_VIEWPORTS {
            cache.insert(format!("chat-{ix}"), SavedViewport::FollowTail);
        }

        assert!(cache.get_cloned_and_touch("chat-0").is_some());
        cache.insert("outgoing-new".into(), SavedViewport::FollowTail);

        assert!(cache.by_chat.contains_key("chat-0"));
        assert!(!cache.by_chat.contains_key("chat-1"));
        assert!(cache.by_chat.contains_key("outgoing-new"));
    }

    #[test]
    fn viewport_finalization_waits_for_current_generation_and_stable_layout() {
        let token = ViewportFinalizeToken {
            generation: 7,
            layout_revision: 11,
        };
        assert!(token.still_current(7));
        assert!(!token.still_current(8));
        assert!(token.layout_settled(11));
        assert!(!token.layout_settled(12));
    }

    fn parse(_: &str, text: &str) -> Arc<BlockTree> {
        Arc::new(parse_full(text))
    }

    fn assistant(id: &str, status: MessageStatus, parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts,
            created_at: 0,
            device_id: "dev".into(),
            status: Some(status),
            continuation_of: None,
            duration_ms: None,
        }
    }

    fn text_part(id: &str, text: &str) -> MessagePart {
        MessagePart::Text {
            id: id.into(),
            text: text.into(),
        }
    }

    fn reasoning_part(id: &str, text: &str) -> MessagePart {
        MessagePart::Reasoning {
            id: id.into(),
            text: text.into(),
            thought_ms: None, // slopty
        }
    }

    #[test]
    fn generated_image_owner_candidates_and_same_length_corrections() {
        assert_eq!(
            generated_image_devices("owner", &["host".into(), "local".into(), "owner".into()]),
            vec!["owner", "host", "local"]
        );
        assert_eq!(
            generated_image_devices("", &["host".into(), "host".into(), "".into()]),
            vec!["host"]
        );
        let entries: Vec<SessionMessageEntry> =
            serde_json::from_str(include_str!("../tests/fixtures/generated-images.json")).unwrap();
        let entry = &entries[0];
        let original = entry_fingerprint(entry, false);
        let row_version = rows_for_entry(entry, false, &mut parse)[0].version;
        for field in 0..4 {
            let mut changed = entry.clone();
            if field == 0 {
                changed.device_id = "another-owner".into();
            } else if let MessagePart::Image {
                path,
                name,
                mime_type,
                ..
            } = &mut changed.parts[0]
            {
                match field {
                    1 => *path = path.replace("loaded", "edited"),
                    2 => *name = "corrected.png".into(),
                    _ => *mime_type = "image/gif".into(),
                }
            }
            assert_ne!(entry_fingerprint(&changed, false), original);
            assert_ne!(
                rows_for_entry(&changed, false, &mut parse)[0].version,
                row_version
            );
        }
        for entry in entries {
            let rows = rows_for_entry(&entry, false, &mut parse);
            assert_eq!(rows.len(), 1);
            assert!(rows[0].turn_start);
            assert_eq!(
                rows[0].timestamp.is_some(),
                entry.status == Some(MessageStatus::Complete)
            );
            assert!(rows[0].copy_text.is_none());
        }
    }

    #[gpui::test]
    fn expanded_tool_call_text_drag_selects(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let command = "cat /workspace/src/main.rs";
        struct Detail;
        impl Render for Detail {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = Theme::of(cx).clone();
                div()
                    .w(px(600.0))
                    .child(render::selection_frame_reset_for(7))
                    .child(detail_body(
                        &ToolDetail::Output {
                            lines: vec!["cat /workspace/src/main.rs".into()],
                            truncated_by: 0,
                            stderr_from: None,
                        },
                        None,
                        "probe",
                        &theme,
                    ))
            }
        }
        let (_, cx) = cx.add_window_view(|_, _| Detail);
        cx.run_until_parked();
        let bounds = render::selection_test_bounds("probe-0");
        let start = bounds.origin + gpui::point(px(1.0), bounds.size.height / 2.0);
        let end = start + gpui::point(bounds.size.width + px(40.0), px(0.0));
        cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
        assert_eq!(
            crate::markdown::selection::selected_text().as_deref(),
            Some(command)
        );
        crate::markdown::selection::clear_if_owner("probe-0");
    }


    /// slopty: a picture a tool returned is named by its file; one the agent generated says so.
    #[test]
    fn a_tool_picture_is_named_by_its_file() {
        assert_eq!(image_label("slopty-image:7/toolu_01/0/abc.png", "plot.png"), "Image: plot.png");
        assert_eq!(image_label("/cache/gen/image-1.png", "image-1.png"), "Preview generated image");
        assert_eq!(image_label("slopty-image:7/toolu_01/0/abc.png", " "), "Preview generated image");
    }

    /// slopty: a prompt a jump goes to lands below the titlebar the transcript fades under,
    /// where a sent prompt is held, not behind it.
    #[gpui::test]
    fn a_jump_lands_the_prompt_below_the_titlebar(cx: &mut gpui::TestAppContext) {
        struct Fixture(Entity<Transcript>);
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(self.0.clone())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(true);
        });
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        transcript.update(cx, |this, cx| {
            this.rows =
                (0..60).map(|ix| viewport_row(&format!("row-{ix}"), &format!("entry-{ix}"))).collect();
            this.list.reset(60);
            this.pinned = false;
            cx.notify();
        });
        let (_fixture, cx) = cx.add_window_view(|_, _| Fixture(transcript.clone()));
        cx.run_until_parked();
        transcript.update(cx, |this, cx| this.scroll_to_row(30, cx));
        cx.run_until_parked();
        transcript.read_with(cx, |this, _| {
            let row = this.list.bounds_for_item(30).expect("the row is drawn");
            let below = f32::from(row.top() - this.list.viewport_bounds().top());
            assert!((below - OWN_SEND_TOP_INSET_PX).abs() <= 0.5, "landed {below}px down");
        });
    }

    /// slopty: with motion, a jump glides a step a frame and lands its row below the titlebar,
    /// where it flashes (#13).
    #[gpui::test]
    fn a_jump_glides_on_frames_and_flashes_where_it_lands(cx: &mut gpui::TestAppContext) {
        struct Fixture(Entity<Transcript>);
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(self.0.clone())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(false);
        });
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        transcript.update(cx, |this, cx| {
            this.rows =
                (0..60).map(|ix| viewport_row(&format!("row-{ix}"), &format!("entry-{ix}"))).collect();
            this.list.reset(60);
            this.pinned = false;
            cx.notify();
        });
        let (_fixture, cx) = cx.add_window_view(|_, _| Fixture(transcript.clone()));
        cx.run_until_parked();
        transcript.update(cx, |this, cx| this.scroll_to_row(30, cx));
        assert!(transcript.read_with(cx, |this, _| this.glide.is_some()), "a glide, not a jump");
        // Frames 8 ms apart, as at 120 Hz: the frame that render asks for steps the glide, and
        // nothing else does. It moves on most frames, never back, and lands on its clock, well
        // inside its frames' bound.
        cx.run_until_parked();
        // The row at the viewport's top and how far into it: a landing above its row (a
        // negative offset) is read as the row before's, which the last frames measured.
        let at = |cx: &mut gpui::VisualTestContext| {
            transcript.read_with(cx, |this, _| {
                let top = this.list.logical_scroll_top();
                let into = f32::from(top.offset_in_item);
                if into < 0.0 && top.item_ix > 0 {
                    let before = this.list.offset_for_item(top.item_ix) - this.list.offset_for_item(top.item_ix - 1);
                    (top.item_ix - 1, f32::from(before) + into)
                } else {
                    (top.item_ix, into)
                }
            })
        };
        let mut seen = vec![at(cx)];
        let mut frames = 0;
        while transcript.read_with(cx, |this, _| this.glide.is_some()) {
            assert!(frames < 120, "a 500 ms glide lands on its clock, not at its frames' bound");
            cx.executor().advance_clock(std::time::Duration::from_millis(8));
            assert!(cx.update(|window, cx| window.simulate_next_frame(cx)) > 0, "a frame asked for");
            cx.run_until_parked();
            seen.push(at(cx));
            frames += 1;
        }
        assert!(frames >= 40, "it took {frames} frames: a glide, not a jump");
        // From the list's end up to row 30: every frame's step is up, or none.
        assert!(seen.windows(2).all(|pair| pair[1] <= pair[0]), "it never moves back: {seen:?}");
        let moved = seen.windows(2).filter(|pair| pair[1] != pair[0]).count();
        assert!(moved * 2 > frames, "it moved on most frames, {moved} of {frames}");
        cx.run_until_parked();
        transcript.read_with(cx, |this, _| {
            assert!(this.glide.is_none(), "the glide landed");
            let row = this.list.bounds_for_item(30).expect("the row is drawn");
            let below = f32::from(row.top() - this.list.viewport_bounds().top());
            assert!((below - OWN_SEND_TOP_INSET_PX).abs() <= 0.5, "landed {below}px down");
            assert_eq!(
                this.landed.as_ref().map(|(id, _, _)| id.as_ref()),
                Some("row-30"),
                "where it landed flashes"
            );
        });
    }

    /// slopty: a frame the view asked for can be lost (GPUI's retained check draws the window
    /// again and drops the callbacks that draw asked for) while the view keeps its note that it
    /// asked. The next draw past the patience asks again, so the glide still lands; a note that
    /// waited for good froze the chat after a send.
    #[gpui::test]
    fn a_glide_whose_frame_was_lost_asks_again_and_lands(cx: &mut gpui::TestAppContext) {
        struct Fixture(Entity<Transcript>);
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(self.0.clone())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(false);
        });
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        transcript.update(cx, |this, cx| {
            this.rows =
                (0..60).map(|ix| viewport_row(&format!("row-{ix}"), &format!("entry-{ix}"))).collect();
            this.list.reset(60);
            this.pinned = false;
            cx.notify();
        });
        let (_fixture, cx) = cx.add_window_view(|_, _| Fixture(transcript.clone()));
        cx.run_until_parked();
        transcript.update(cx, |this, cx| this.scroll_to_row(30, cx));
        cx.run_until_parked();
        // The frame the draw asked for is lost: its answer no longer matches the view's note,
        // which still says it waits.
        let top = |cx: &mut gpui::VisualTestContext| {
            transcript.read_with(cx, |this, _| {
                let top = this.list.logical_scroll_top();
                (top.item_ix, f32::from(top.offset_in_item))
            })
        };
        transcript.update(cx, |this, cx| {
            assert!(this.glide.is_some() && this.glide_scheduled.waiting(), "the glide asked for a frame");
            this.glide_scheduled = FrameAsk { token: this.glide_scheduled.token.wrapping_add(7), since: Some(crate::clock::now(cx)) };
        });
        let stuck = top(cx);
        cx.executor().advance_clock(Duration::from_millis(8));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        assert_eq!(top(cx), stuck, "the lost frame stepped nothing");
        assert!(transcript.read_with(cx, |this, _| this.glide.is_some()));
        // Anything that draws the chat past the patience asks again; the glide lands.
        cx.executor().advance_clock(FRAME_ASK_PATIENCE);
        transcript.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let mut frames = 0;
        while transcript.read_with(cx, |this, _| this.glide.is_some()) {
            assert!(frames < 120, "the glide never landed: its frame was waited on for good");
            cx.executor().advance_clock(Duration::from_millis(8));
            assert!(cx.update(|window, cx| window.simulate_next_frame(cx)) > 0, "a frame asked for");
            cx.run_until_parked();
            frames += 1;
        }
        transcript.read_with(cx, |this, _| {
            let row = this.list.bounds_for_item(30).expect("the row is drawn");
            let below = f32::from(row.top() - this.list.viewport_bounds().top());
            assert!((below - OWN_SEND_TOP_INSET_PX).abs() <= 0.5, "landed {below}px down");
        });
    }

    /// slopty: one ask waits at a time; past the patience it is asked again, and only the
    /// latest ask's frame answers it.
    #[test]
    fn a_frame_ask_waits_its_patience_then_asks_again() {
        let t0 = Instant::now();
        let mut ask = FrameAsk::default();
        let first = ask.ask(t0).expect("nothing waits yet");
        assert_eq!(ask.ask(t0 + Duration::from_millis(16)), None, "one ask waits at a time");
        let second = ask.ask(t0 + FRAME_ASK_PATIENCE).expect("past the patience it asks again");
        assert!(!ask.answered(first), "the older frame's late answer is ignored");
        assert!(ask.waiting(), "and the newer ask still waits");
        assert!(ask.answered(second));
        assert!(!ask.waiting());
        assert!(!ask.answered(second), "an ask is answered once");
        assert!(ask.ask(t0 + FRAME_ASK_PATIENCE).is_some(), "an answered ask asks again at once");
    }

    #[gpui::test]
    fn generated_image_click_opens_lightbox_and_escape_restores_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(64, 48)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::attachments::store_loaded_for(
            &crate::attachments::AttachmentKey::new(
                "preview-owner",
                "/fixture/preview.png",
                Some("image/png"),
            ),
            "generated.png".into(),
            Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                bytes.into_inner(),
            )),
        );
        struct PreviewFixture(Entity<Transcript>);
        impl Render for PreviewFixture {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                if self.0.read(cx).attachment_preview.is_some() {
                    self.0.clone().into_any_element()
                } else {
                    self.0.update(cx, |this, cx| {
                        this.render_generated_image(
                            &"preview".into(),
                            "preview-owner",
                            "/fixture/preview.png",
                            "generated.png",
                            "image/png",
                            cx,
                        )
                    })
                }
            }
        }
        let (fixture, cx) = cx.add_window_view(|_, _| PreviewFixture(transcript.clone()));
        cx.run_until_parked();
        cx.simulate_click(gpui::point(px(20.0), px(20.0)), gpui::Modifiers::default());
        let return_focus = transcript.read_with(cx, |this, _| {
            assert!(this.attachment_preview.is_some());
            this.attachment_preview_return_focus
                .clone()
                .expect("preview remembers the image button focus")
        });
        // Paint the production transcript, including its shared lightbox and
        // close callback, so Escape exercises actual focus restoration.
        fixture.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        transcript.read_with(cx, |this, _| assert!(this.attachment_preview.is_none()));
        cx.update(|window, _| assert!(return_focus.is_focused(window)));
    }

    #[gpui::test]
    fn generated_image_fixture_uses_cache_and_retries(cx: &mut gpui::TestAppContext) {
        use crate::attachments::*;
        forget_for_test(); // slopty: no load the last seed claimed
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            let state = cx.new(|_| AppState::new());
            let transcript = cx.new(|cx| Transcript::new(state, cx));
            let entries: Vec<SessionMessageEntry> =
                serde_json::from_str(include_str!("../tests/fixtures/generated-images.json"))
                    .unwrap();
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(64, 48)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            let image = Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                bytes.into_inner(),
            ));
            store_loaded_for(
                &AttachmentKey::new(
                    "fixture-owner",
                    "/fixture/generated-loaded.png",
                    Some("image/png"),
                ),
                "generated.png".into(),
                image,
            );
            assert!(begin_load_for(&AttachmentKey::new(
                "fixture-owner",
                "/fixture/generated-loading.png",
                Some("image/png")
            )));
            store_error_for(&AttachmentKey::new(
                "fixture-owner",
                "/fixture/generated-unavailable.png",
                Some("image/png"),
            ));
            transcript.update(cx, |this, cx| {
                this.rows = entries
                    .iter()
                    .flat_map(|entry| rows_for_entry(entry, false, &mut parse))
                    .collect();
                for (ix, expected) in ["loaded", "loading", "error"].into_iter().enumerate() {
                    let RowKind::GeneratedImage {
                        owner,
                        path,
                        mime_type,
                        ..
                    } = &this.rows[ix].kind
                    else {
                        panic!("image row")
                    };
                    let (owner, path, mime_type) = (owner.clone(), path.clone(), mime_type.clone());
                    let devices = this.generated_attachment_device_ids(&owner, cx);
                    let snapshot = this.attachment_state(&devices, &path, Some(&mime_type), cx);
                    match expected {
                        "loaded" => assert!(matches!(snapshot, AttachmentSnapshot::Loaded(_))),
                        "loading" => assert!(matches!(snapshot, AttachmentSnapshot::Loading)),
                        _ => assert!(matches!(snapshot, AttachmentSnapshot::Error { .. })),
                    }
                }
                assert!(
                    this.attachment_loads.is_empty(),
                    "cache repaint must not fetch"
                );
                assert_eq!(this.attachment_retries.len(), 1);
                let keys = this.protected_attachment_keys(cx);
                for entry in &entries {
                    let MessagePart::Image { path, .. } = &entry.parts[0] else {
                        unreachable!()
                    };
                    assert!(!keys.contains(&("fixture-owner".into(), path.clone())));
                }
                this.refresh_protected_attachments(cx);
                this.rows.clear();
                assert!(this.protected_attachment_keys(cx).is_empty());
                this.refresh_protected_attachments(cx);
            });
        });
    }

    #[test]
    fn generated_image_builds_one_row_outside_tools_and_copy() {
        let image = MessagePart::Image {
            id: "i:image".into(),
            path: "/uploads/i.png".into(),
            name: "generated.png".into(),
            mime_type: "image/png".into(),
        };
        let entry = assistant("a", MessageStatus::Complete, vec![image.clone()]);
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].turn_start);
        assert!(rows[0].copy_text.is_none());
        assert!(matches!(&rows[0].kind, RowKind::GeneratedImage { owner, .. } if owner == "dev"));
        let entry = assistant(
            "a",
            MessageStatus::Complete,
            vec![
                text_part("t", "before"),
                tool_part("i", "generate"),
                image,
                text_part("t2", "after"),
            ],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[1].kind, RowKind::ToolGroup { .. }));
        assert!(matches!(rows[2].kind, RowKind::GeneratedImage { .. }));
    }

    #[test]
    fn reasoning_joins_the_tool_group_accordion() {
        // Thought → tool → thought → tool folds into ONE group row (user
        // request: the thought process lives inside the combined accordion),
        // and the collapsed summary names the thinking.
        let entry = assistant(
            "a1",
            MessageStatus::Complete,
            vec![
                reasoning_part("r0", "planning the first step"),
                // slopty: a listing counts apart ("listed 1 directory"); a command here.
                tool_part("t1", "cargo test"),
                reasoning_part("r2", "now the second step"),
                tool_part("t3", "pwd"),
            ],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 1, "one combined accordion row");
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("expected a tool group");
        };
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0].kind, ToolItemKind::Thought);
        assert_eq!(tools[2].kind, ToolItemKind::Thought);
        assert_eq!(tools[1].kind, ToolItemKind::Call);
        assert_eq!(tools[3].kind, ToolItemKind::Call);
        // Thought chips carry their text as a styled-line detail with an
        // ANALYTIC height, so the group's fold tween covers them.
        assert!(matches!(
            tools[0].detail.as_deref(),
            Some(ToolDetail::Thought { lines, .. }) if !lines.is_empty()
        ));
        let summary = tool_group_summary(&tools);
        assert!(summary.starts_with("Thought 2 times"), "{summary}");
        assert!(summary.contains("2 commands"), "{summary}");
        // slopty: a thought that says how long it took is named by it.
        let timed = assistant(
            "a2",
            MessageStatus::Complete,
            vec![
                MessagePart::Reasoning {
                    id: "r".into(),
                    text: "planning".into(),
                    thought_ms: Some(12_000),
                },
                tool_part("t", "cargo test"),
            ],
        );
        let rows = rows_for_entry(&timed, false, &mut parse);
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("expected a tool group");
        };
        assert_eq!(tool_group_summary(tools), "Thought for 12s and ran 1 command");
        // slopty: under a second, it thought briefly.
        let brief = assistant(
            "a2",
            MessageStatus::Complete,
            vec![
                MessagePart::Reasoning { id: "r".into(), text: "planning".into(), thought_ms: Some(400) },
                tool_part("t", "cargo test"),
            ],
        );
        let rows = rows_for_entry(&brief, false, &mut parse);
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("expected a tool group");
        };
        assert_eq!(tool_group_summary(tools), "Thought briefly and ran 1 command");

        // A lone thought is still an accordion (with the group tween), named
        // plainly.
        let entry = assistant(
            "a2",
            MessageStatus::Complete,
            vec![reasoning_part("r0", "just thinking")],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 1);
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("expected a tool group");
        };
        assert_eq!(tool_group_summary(&tools), "Thought once");

        // Empty reasoning renders nothing.
        let entry = assistant(
            "a3",
            MessageStatus::Complete,
            vec![reasoning_part("r0", "   ")],
        );
        assert!(rows_for_entry(&entry, false, &mut parse).is_empty());
    }

    #[test]
    fn a_live_thought_runs_until_a_part_follows_it() {
        let entry = assistant(
            "a1",
            MessageStatus::Streaming,
            vec![reasoning_part("r0", "thinking hard")],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("expected a tool group");
        };
        assert!(!tools[0].resolved, "the live tail is still thinking");

        let entry = assistant(
            "a2",
            MessageStatus::Streaming,
            vec![
                reasoning_part("r0", "thinking hard"),
                text_part("t1", "answer"),
            ],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("expected a tool group");
        };
        assert!(tools[0].resolved, "a followed thought is settled");
    }

    /// slopty: a turn's rows are its records in the order they were written: its narration where
    /// it was said, each run of calls and thoughts one group line keyed by its first call, an
    /// error where it happened. A group grows in place under its key, and the turn's end moves
    /// no row and changes no row's container (`docs/decisions/inspector.md`, "Rows in record
    /// order").
    #[test]
    fn a_turns_rows_keep_record_order_and_their_place_when_it_ends() {
        let ids = |rows: &[Row]| rows.iter().map(|row| row.id.to_string()).collect::<Vec<_>>();
        let error = MessagePart::Error {
            id: "e3".into(),
            message: "Overloaded, trying again".into(),
            detail: None,
        };
        let parts = vec![
            text_part("t0", "Looking at the build."),
            tool_part("c1", "cargo build"),
            reasoning_part("r2", "It failed on a lint."),
            error,
            tool_part("c4", "cargo clippy"),
            text_part("t5", "Fixed it."),
        ];
        let early = assistant("a1", MessageStatus::Streaming, parts[..2].to_vec());
        assert_eq!(ids(&rows_for_entry(&early, false, &mut parse)), ["a1#t0.0", "a1#g:c1"]);
        let grown = assistant("a1", MessageStatus::Streaming, parts[..3].to_vec());
        assert_eq!(
            ids(&rows_for_entry(&grown, false, &mut parse)),
            ["a1#t0.0", "a1#g:c1"],
            "a thought joins its group under the group's own key"
        );
        let want = ["a1#t0.0", "a1#g:c1", "a1#e3", "a1#g:c4", "a1#t5.0"];
        let live = assistant("a1", MessageStatus::Streaming, parts.clone());
        assert_eq!(ids(&rows_for_entry(&live, false, &mut parse)), want, "live, in record order");
        let done = assistant("a1", MessageStatus::Complete, parts);
        assert_eq!(ids(&rows_for_entry(&done, false, &mut parse)), want, "ended, nothing moved");
    }

    #[test]
    fn a_tool_fold_shows_its_contents_after_its_height() {
        let at = Instant::now();
        let early = tool_fold(true, Some(at), at + Duration::from_millis(60), false);
        assert!(early.share > 0.4 && early.opacity < 0.2 && early.rise > 0.0, "{early:?}");
        assert!(early.moving);
        let open = tool_fold(true, Some(at), at + Duration::from_millis(301), false);
        assert_eq!((open.share, open.opacity, open.rise, open.moving), (1.0, 1.0, 0.0, false));
        let closing = tool_fold(false, Some(at), at + Duration::from_millis(150), false);
        assert!(closing.opacity <= 0.0 && closing.share < 1.0 && closing.moving, "{closing:?}");
        let still = tool_fold(true, Some(at), at, true);
        assert_eq!((still.share, still.opacity, still.moving), (1.0, 1.0, false), "reduced motion lands");
    }

    fn thought_of(text: &str) -> Vec<Vec<InlineRun>> {
        thought_lines(&parse_full(text))
    }

    fn line_chars(line: &[InlineRun]) -> usize {
        line.iter().map(|r| r.text.chars().count()).sum()
    }

    fn line_string(line: &[InlineRun]) -> String {
        line.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn codex_summary_paragraphs_render_as_separate_styled_lines() {
        let lines = thought_of("**Implementing file badges**\n\n**Preparing fixture screenshots**");
        assert_eq!(
            lines
                .iter()
                .map(|line| line_string(line))
                .collect::<Vec<_>>(),
            [
                "Implementing file badges",
                "",
                "Preparing fixture screenshots"
            ]
        );
        for ix in [0, 2] {
            assert!(
                lines[ix]
                    .iter()
                    .filter(|run| !run.text.is_empty())
                    .all(|run| run.style.bold)
            );
        }
    }

    #[test]
    fn thought_wrap_is_word_aware_and_bounded() {
        let lines = thought_of("one two three");
        assert_eq!(lines.len(), 1);
        assert_eq!(line_string(&lines[0]), "one two three");
        let long = "word ".repeat(200);
        let lines = thought_of(&long);
        assert!(lines.iter().all(|l| line_chars(l) <= THOUGHT_WRAP_COLS));
        assert!(lines.len() > 5);
        let pathological = "x".repeat(300);
        let lines = thought_of(&pathological);
        assert!(lines.iter().all(|l| line_chars(l) <= THOUGHT_WRAP_COLS));
        // A word glued across style boundaries wraps as ONE unit — no line
        // may split inside `**bold**tail`.
        let glued = format!("{} **bold**tail", "word ".repeat(30));
        let lines = thought_of(&glued);
        let joined: Vec<String> = lines.iter().map(|l| line_string(l)).collect();
        assert!(joined.iter().any(|l| l.ends_with("boldtail")), "{joined:?}");
    }

    #[test]
    fn thought_markdown_styles_instead_of_literal_markers() {
        // The exact user report: `**bold**` markers showed as glyphs.
        let lines = thought_of("**Planning rollback** then *checking* `parse` [docs](https://d)");
        assert_eq!(lines.len(), 1);
        let flat = line_string(&lines[0]);
        assert!(
            !flat.contains('*') && !flat.contains('`') && !flat.contains('['),
            "{flat}"
        );
        let line = &lines[0];
        assert!(
            line.iter()
                .any(|r| r.style.bold && r.text.contains("Planning rollback")),
            "bold run survives: {line:?}"
        );
        assert!(
            line.iter()
                .any(|r| r.style.italic && r.text.contains("checking"))
        );
        assert!(
            line.iter()
                .any(|r| r.style.code && r.text.contains("parse"))
        );
        assert!(
            line.iter()
                .any(|r| r.style.link.is_some() && r.text.contains("docs"))
        );
    }

    #[test]
    fn thought_math_keeps_its_delimiters_as_code() {
        let lines = thought_of(r"so $x_1 + y$ holds");
        assert_eq!(line_string(&lines[0]), r"so $x_1 + y$ holds");
        assert!(
            lines[0]
                .iter()
                .any(|r| r.style.code && !r.style.math && r.text.trim_end() == r"$x_1 + y$"),
            "{:?}",
            lines[0]
        );
    }

    #[test]
    fn thought_blocks_flatten_structurally() {
        let lines = thought_of("# Head\n\npara\n\n- one\n- two\n\n```rust\nlet x = 1;\n```");
        let flat: Vec<String> = lines.iter().map(|l| line_string(l)).collect();
        // slopty: a heading reads as its prose: same size, not bold.
        assert!(lines[0].iter().any(|r| r.text.contains("Head")));
        assert!(lines[0].iter().all(|r| !r.style.bold), "{:?}", lines[0]);
        // Blank separator rows between top-level blocks; tight list inside.
        assert_eq!(flat[1], "");
        assert_eq!(flat[2], "para");
        assert_eq!(flat[4], "• one");
        assert_eq!(flat[5], "• two");
        // Code lines verbatim, styled as code (mono at render).
        assert!(
            lines
                .last()
                .unwrap()
                .iter()
                .any(|r| r.style.code && r.text == "let x = 1;"),
            "{flat:?}"
        );
    }

    fn tool_part(id: &str, command: &str) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call: ToolCall::Exec {
                command: command.into(),
            },
            is_error: false,
            resolved: true,
            output: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            facts: None,
        }
    }

    const MD: &str = "# Title\n\npara one\n\n```rust\nlet x = 1;\n```";

    #[test]
    fn live_entry_splits_per_block_with_id_continuity() {
        // Live rows split per block exactly like completed ones (the list
        // virtualizes them — the fading tail is the only per-frame work).
        let live = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", MD)]);
        let live_rows = rows_for_entry(&live, false, &mut parse);
        assert_eq!(live_rows.len(), 3, "one live row per top-level block");
        assert!(
            live_rows
                .iter()
                .all(|r| matches!(r.kind, RowKind::LiveMarkdown { .. }))
        );
        assert_eq!(live_rows[0].id.as_ref(), "m1#t0.0");
        assert_eq!(live_rows[2].id.as_ref(), "m1#t0.2");

        let done = assistant("m1", MessageStatus::Complete, vec![text_part("t0", MD)]);
        let done_rows = rows_for_entry(&done, false, &mut parse);
        assert_eq!(done_rows.len(), 3, "three top-level blocks");
        // Every block row keeps its id across the flip — no flicker on handoff.
        for (live, done) in live_rows.iter().zip(&done_rows) {
            assert_eq!(live.id, done.id);
            // The flip changes the version even at identical text (the
            // streaming bit), forcing a splice.
            assert_ne!(live.version, done.version);
        }
        assert!(matches!(
            done_rows[0].kind,
            RowKind::Markdown { block_ix: 0, .. }
        ));
    }

    #[test]
    fn live_commit_changes_only_tail_row_versions() {
        // Streaming commit: appending to the last block leaves every settled
        // block row's (id, version) untouched — the diff splices only the tail.
        let t1 = "para one\n\npara two\n\npara three";
        let t2 = "para one\n\npara two\n\npara three grows here";
        let live1 = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", t1)]);
        let live2 = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", t2)]);
        let r1 = rows_for_entry(&live1, false, &mut parse);
        let r2 = rows_for_entry(&live2, false, &mut parse);
        assert_eq!(r1.len(), 3);
        assert_eq!(r2.len(), 3);
        assert_eq!(r1[0].version, r2[0].version, "settled block untouched");
        assert_eq!(r1[1].version, r2[1].version, "settled block untouched");
        assert_ne!(r1[2].version, r2[2].version, "tail block respliced");
        assert_eq!(diff_rows(&r1, &r2), Some((2..3, 1)));
    }

    #[test]
    fn split_sibling_gaps_match_live_internal_spacing() {
        // The live row spaces its internal blocks by MD_BLOCK_GAP; after the
        // live→split handoff the same boundaries are inter-row gaps. They must
        // be identical or the whole message jumps at completion.
        let done = assistant(
            "m1",
            MessageStatus::Complete,
            vec![
                text_part("t0", MD),
                tool_part("a", "ls"),
                text_part("t1", "tail para"),
            ],
        );
        let rows = rows_for_entry(&done, false, &mut parse);
        // Rows: t0.0, t0.1, t0.2 (three MD blocks), g0, t1.0.
        assert_eq!(rows.len(), 5);
        // Sibling markdown blocks from the same part: md block gap.
        assert_eq!(top_gap_for(Some(&rows[0]), &rows[1]), render::MD_BLOCK_GAP);
        assert_eq!(top_gap_for(Some(&rows[1]), &rows[2]), render::MD_BLOCK_GAP);
        // Markdown → tool group and tool group → next part: larger boundary.
        assert_eq!(top_gap_for(Some(&rows[2]), &rows[3]), Theme::SPACE_MD);
        assert_eq!(top_gap_for(Some(&rows[3]), &rows[4]), Theme::SPACE_MD);
        // Turn starts get the turn gap regardless.
        assert_eq!(top_gap_for(None, &rows[0]), Theme::SPACE_LG);
    }

    #[test]
    fn consecutive_tools_fold_into_groups_between_text() {
        let entry = assistant(
            "m2",
            MessageStatus::Complete,
            vec![
                text_part("t0", "before"),
                tool_part("a", "ls"),
                tool_part("b", "pwd"),
                text_part("t1", "after"),
                tool_part("c", "make"),
            ],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_ref()).collect();
        assert_eq!(ids, ["m2#t0.0", "m2#g:a", "m2#t1.0", "m2#g:c"]);
        let RowKind::ToolGroup { tools, .. } = &rows[1].kind else {
            panic!("group expected")
        };
        assert_eq!(tools.len(), 2);
        assert!(rows[0].turn_start && !rows[1].turn_start);
    }

    fn agent_part(id: &str, description: &str) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call: ToolCall::Unknown {
                name: format!("Agent: {description}"),
                input: Some(serde_json::json!({ "description": description })),
            },
            is_error: false,
            resolved: true,
            output: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: Some(format!("chat--sub--{id}")),
            subagent_status: Some(SubagentStatus::Running),
            subagent_tail: None,
            facts: None,
        }
    }

    #[test]
    fn agent_calls_split_out_of_ordinary_tool_groups() {
        // Agent/spawn chips must not share a collapse with Reads/Runs: a
        // lone Agent used to hide behind "Called 1 tool", and a mixed
        // group hid the running subagent until the user opened the fold.
        let entry = assistant(
            "m-agent",
            MessageStatus::Complete,
            vec![
                text_part("t0", "before"),
                tool_part("a", "ls"),
                tool_part("b", "pwd"),
                agent_part("s1", "Map URL import ingest path"),
                tool_part("c", "make"),
                agent_part("s2", "Audit the fold path"),
                agent_part("s3", "Verify the commit cadence"),
                text_part("t1", "after"),
            ],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_ref()).collect();
        assert_eq!(
            ids,
            [
                "m-agent#t0.0",
                "m-agent#g:a",
                "m-agent#g:s1",
                "m-agent#g:c",
                "m-agent#g:s2",
                "m-agent#t1.0",
            ]
        );

        let RowKind::ToolGroup { tools, .. } = &rows[1].kind else {
            panic!("ordinary group expected")
        };
        assert_eq!(tools.len(), 2);
        assert!(tool_group_collapses(tools));

        let RowKind::ToolGroup { tools, .. } = &rows[2].kind else {
            panic!("agent group expected")
        };
        assert_eq!(tools.len(), 1);
        assert!(!tool_group_collapses(tools));
        assert!(is_agent_tool(&tools[0]));

        let RowKind::ToolGroup { tools, .. } = &rows[3].kind else {
            panic!("ordinary group expected")
        };
        assert_eq!(tools.len(), 1);
        assert!(tool_group_collapses(tools));

        let RowKind::ToolGroup { tools, .. } = &rows[4].kind else {
            panic!("consecutive agents share a group")
        };
        assert_eq!(tools.len(), 2);
        assert!(!tool_group_collapses(tools));
        assert!(tools.iter().all(is_agent_tool));
    }

    #[test]
    fn stray_subagent_ref_on_a_run_chip_stays_an_ordinary_tool() {
        // Docs written before the claude-driver fix carry subagent refs on
        // ordinary Run chips (a background shell's task_notification was
        // mis-tagged as subagent traffic). The ref alone must not change the
        // chip's genus: it folds with its neighbors and renders as a plain
        // tool, never as a spawn link to a doc that was never created.
        let mut stray = tool_part("b", "git clone …");
        if let MessagePart::Tool {
            subagent_ref,
            subagent_status,
            ..
        } = &mut stray
        {
            *subagent_ref = Some("chat--sub--b".into());
            *subagent_status = Some(SubagentStatus::Done);
        }
        let entry = assistant(
            "m-stray",
            MessageStatus::Complete,
            vec![tool_part("a", "ls"), stray, tool_part("c", "make")],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 1, "one folded group, no agent split");
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("tool group expected")
        };
        assert_eq!(tools.len(), 3);
        assert!(tool_group_collapses(tools));
        assert!(tools.iter().all(|t| !is_agent_tool(t)));
        assert!(tools.iter().all(|t| !is_spawn_link(t)));
    }

    #[test]
    fn lone_completed_agent_stays_uncollapsed() {
        let entry = assistant(
            "m-lone",
            MessageStatus::Complete,
            vec![agent_part("s1", "scan repo")],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 1);
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!("agent group expected")
        };
        assert_eq!(tools.len(), 1);
        assert!(!tool_group_collapses(tools), "no 'Called 1 tool' wrap");
    }

    #[test]
    fn pre_spawn_agent_name_is_enough_to_split() {
        // Before the engine stamps subagent_ref the chip is already named
        // "Agent: …" — that genus must split, or the spawn hides until the
        // first tagged event.
        let mut part = agent_part("s1", "scan repo");
        if let MessagePart::Tool {
            subagent_ref,
            subagent_status,
            ..
        } = &mut part
        {
            *subagent_ref = None;
            *subagent_status = None;
        }
        let entry = assistant(
            "m-pre",
            MessageStatus::Complete,
            vec![tool_part("a", "ls"), part],
        );
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 2);
        let RowKind::ToolGroup { tools, .. } = &rows[0].kind else {
            panic!()
        };
        assert!(tool_group_collapses(tools));
        let RowKind::ToolGroup { tools, .. } = &rows[1].kind else {
            panic!()
        };
        assert!(!tool_group_collapses(tools));
        assert!(is_agent_call(&tools[0].call));
    }

    /// slopty: a group opens only as the person asks: not while its calls run, not as more
    /// arrive, not at the turn's end (`docs/decisions/inspector.md`, "One fold depth").
    #[gpui::test]
    fn a_group_never_opens_by_itself(cx: &mut gpui::TestAppContext) {
        with_tool_group_navigation(cx, |state, transcript, cx| {
            let mut parts = vec![text_part("t0", "hi"), tool_part("a", "ls"), tool_part("b", "pwd")];
            let shown = |status: MessageStatus, parts: &[MessagePart], cx: &mut gpui::App| {
                state.update(cx, |state, _| {
                    state.transcript = vec![assistant("m3", status, parts.to_vec())];
                    state.transcript_revision += 1;
                });
                transcript.update(cx, |this, cx| {
                    this.sync(cx);
                    let row = this.rows.iter().find(|row| row.id.as_ref() == "m3#g:a").unwrap().clone();
                    let RowKind::ToolGroup { tools, summary } = &row.kind else { unreachable!() };
                    let _ = this.render_tool_group(&row.id, tools, summary, &Theme::dark(), cx);
                    this.tool_group_reveals[&row.id].rendered_open
                })
            };
            assert_eq!(shown(MessageStatus::Streaming, &parts, cx), Some(false), "while it runs");
            parts.push(tool_part("c", "make"));
            assert_eq!(shown(MessageStatus::Streaming, &parts, cx), Some(false), "as a call arrives");
            assert_eq!(shown(MessageStatus::Complete, &parts, cx), Some(false), "at the turn's end");
        });
    }

    #[test]
    fn user_rows_and_echo_versions() {
        let mut entry = assistant("u1", MessageStatus::Complete, vec![]);
        entry.role = MessageRole::User;
        entry.status = None;
        entry.parts = vec![text_part("t0", "hello")];
        let confirmed = rows_for_entry(&entry, false, &mut parse);
        let echoed = rows_for_entry(&entry, true, &mut parse);
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].id, echoed[0].id);
        // Pending → confirmed changes the version so the row re-renders.
        assert_ne!(confirmed[0].version, echoed[0].version);
        assert!(matches!(
            &echoed[0].kind,
            RowKind::User { pending: true, .. }
        ));
    }

    // Exercise the real Transcript handlers with GPUI's Linux headless
    // platform; no renderer, display server, or test-only dependency needed.
    #[cfg(target_os = "linux")]
    mod user_fold_scroll {
        use super::*;

        fn with_transcript(test: impl FnOnce(&mut Transcript, &mut Context<Transcript>) + 'static) {
            gpui_platform::headless().run(move |cx| {
                let state = cx.new(|_| AppState::new());
                let transcript = cx.new(|cx| Transcript::new(state, cx));
                transcript.update(cx, test);
                // Quit after the platform loop starts; calloop resets its
                // stop flag on entry, so quitting in the launch hook hangs.
                cx.spawn(async move |cx| {
                    cx.update(|cx| cx.quit());
                })
                .detach();
            });
        }

        struct CachedTranscript(Entity<Transcript>);

        impl Render for CachedTranscript {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.0
                    .clone()
                    .cached(gpui::StyleRefinement::default().size_full())
            }
        }

        fn with_window(
            test: impl FnOnce(Entity<Transcript>, gpui::WindowHandle<CachedTranscript>, &mut gpui::App)
            + 'static,
        ) {
            gpui_platform::headless().run(move |cx| {
                cx.set_global(Theme::dark());
                let state = cx.new(|_| AppState::new());
                let window = cx
                    .open_window(
                        gpui::WindowOptions {
                            window_bounds: Some(gpui::WindowBounds::Windowed(Bounds::new(
                                Point::default(),
                                gpui::size(px(1000.0), px(800.0)),
                            ))),
                            ..Default::default()
                        },
                        |_, cx| {
                            let transcript = cx.new(|cx| Transcript::new(state, cx));
                            cx.new(|_| CachedTranscript(transcript))
                        },
                    )
                    .unwrap();
                let transcript = window.entity(cx).unwrap().read(cx).0.clone();
                test(transcript, window, cx);
                cx.spawn(async move |cx| {
                    cx.update(|cx| cx.quit());
                })
                .detach();
            });
        }

        fn draw(window: gpui::WindowHandle<CachedTranscript>, cx: &mut gpui::App) {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            })
            .unwrap();
        }

        #[test]
        fn conversation_width_reflows_streaming_text_without_restarting_animations() {
            with_window(|transcript, window, cx| {
                let dir = tempfile::tempdir().unwrap();
                crate::settings::init(Default::default(), dir.path(), cx);
                let text = "Streaming content should wrap at the configured conversation width. "
                    .repeat(80);
                let mut entries = vec![assistant(
                    "reply",
                    MessageStatus::Streaming,
                    vec![text_part("body", &text)],
                )];
                transcript.update(cx, |this, cx| {
                    feed(this, entries.clone(), cx);
                    this.pinned = false;
                    this.list.scroll_to(ListOffset {
                        item_ix: 0,
                        offset_in_item: px(0.0),
                    });
                });
                draw(window, cx);
                let original_veil =
                    transcript.read(cx).veils[&SharedString::from("reply#body.0")].clone();
                crate::settings::set_conversation_width(
                    crate::settings::ConversationWidth::Default,
                    cx,
                );
                draw(window, cx);
                let narrow = transcript
                    .read(cx)
                    .list
                    .bounds_for_item(0)
                    .unwrap()
                    .size
                    .height;
                crate::settings::set_conversation_width(
                    crate::settings::ConversationWidth::Full,
                    cx,
                );
                draw(window, cx);
                let this = transcript.read(cx);
                let wide = this.list.bounds_for_item(0).unwrap().size.height;
                assert!(
                    wide < narrow,
                    "width change must remeasure wrapped rows: {wide:?} vs {narrow:?}"
                );
                assert!(Rc::ptr_eq(
                    &original_veil,
                    &this.veils[&SharedString::from("reply#body.0")]
                ));
                assert!(!this.pinned);
                assert_eq!(this.list.logical_scroll_top().item_ix, 0);
                assert!(this.list.logical_scroll_top().offset_in_item.abs() <= px(1.0));
                let bounds = render::selection_test_bounds("reply#body.0:0");
                assert!(
                    bounds.size.width <= px(904.0),
                    "content must fit a 1000px viewport with 48px gutters"
                );
                entries[0].parts.push(tool_part("live-tool", "pwd"));
                transcript.update(cx, |this, cx| {
                    feed(this, entries, cx);
                    assert!(
                        this.tool_group_reveals[&SharedString::from("reply#g:live-tool")].starts[0]
                            .is_some()
                    );
                    this.pinned = true;
                    this.list.scroll_to(ListOffset {
                        item_ix: this.list.item_count(),
                        offset_in_item: px(0.0),
                    });
                });
                crate::settings::set_conversation_width(
                    crate::settings::ConversationWidth::Default,
                    cx,
                );
                for _ in 0..80 {
                    tick(&transcript, window, cx);
                }
                assert!(transcript.read(cx).distance_from_bottom() <= 1.0);
            });
        }

        #[test]
        fn replay_keeps_unpainted_opening_text_seeded_and_new_text_live() {
            with_window(|transcript, window, cx| {
                let history = vec![assistant(
                    "reply",
                    MessageStatus::Streaming,
                    vec![text_part("body", "historial fuera de pantalla")],
                )];
                transcript.update(cx, |this, cx| {
                    this.state.update(cx, |state, cx| {
                        state.select_chat(Some("chat".into()), cx);
                        state
                            .receive_transcript_update(
                                slopty_view_doc::TranscriptUpdate {
                                    frame: slopty_view_doc::TranscriptFrame::reset(&history),
                                    replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(
                                        &history,
                                    )),
                                },
                                cx,
                            )
                            .unwrap();
                    });
                    this.sync(cx);
                    assert!(this.veils.is_empty(), "opening text has not been painted");
                });
                let recovered = assistant(
                    "recovered",
                    MessageStatus::Complete,
                    vec![text_part("old", "otro bloque histórico")],
                );
                let mut next = history.clone();
                next[0].parts.push(text_part("fresh", "texto en vivo"));
                next.push(recovered.clone());
                let mut cutoff = history.clone();
                cutoff.push(recovered);
                transcript.update(cx, |this, cx| {
                    this.state.update(cx, |state, cx| {
                        state
                            .receive_transcript_update(
                                slopty_view_doc::TranscriptUpdate {
                                    frame: slopty_view_doc::diff_transcript(&history, &next),
                                    // The RPC must retain its opening cutoff when
                                    // publishing subsequent changed-part history.
                                    replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(
                                        &cutoff,
                                    )),
                                },
                                cx,
                            )
                            .unwrap();
                    });
                    this.sync(cx);
                });
                cx.update_window(window.into(), |_, window, cx| {
                    transcript.update(cx, |this, cx| {
                        let _ = this.render_row(0, window, cx);
                        let _ = this.render_row(1, window, cx);
                        let old = &this.veils[&SharedString::from("reply#body.0")];
                        assert!(
                            old.borrow_mut()
                                .advance(0, "historial fuera de pantalla", Instant::now())
                                .is_empty()
                        );
                        let fresh = &this.veils[&SharedString::from("reply#fresh.0")];
                        let spans = fresh
                            .borrow_mut()
                            .advance(0, "texto en vivo", Instant::now());
                        assert_eq!(spans.len(), 1);
                        assert_eq!(spans[0].0, 0.."texto en vivo".len());
                    });
                })
                .unwrap();
            });
        }

        #[test]
        fn replay_text_prefix_is_visible_while_coalesced_live_suffix_fades() {
            with_window(|transcript, window, cx| {
                let history = vec![assistant(
                    "reply",
                    MessageStatus::Streaming,
                    vec![text_part("body", "café histórico")],
                )];
                let live = vec![assistant(
                    "reply",
                    MessageStatus::Streaming,
                    vec![text_part("body", "café histórico y nuevo\n\nOtro párrafo")],
                )];
                transcript.update(cx, |this, cx| {
                    this.state.update(cx, |state, cx| {
                        state.select_chat(Some("chat".into()), cx);
                        state
                            .receive_transcript_update(
                                slopty_view_doc::TranscriptUpdate {
                                    frame: slopty_view_doc::TranscriptFrame::reset(&history),
                                    replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(
                                        &history,
                                    )),
                                },
                                cx,
                            )
                            .unwrap();
                        state
                            .receive_transcript_update(
                                slopty_view_doc::TranscriptUpdate {
                                    frame: slopty_view_doc::diff_transcript(&history, &live),
                                    replay_baseline: None,
                                },
                                cx,
                            )
                            .unwrap();
                    });
                    this.sync(cx);
                });
                cx.update_window(window.into(), |_, window, cx| {
                    transcript.update(cx, |this, cx| {
                        let _ = this.render_row(0, window, cx);
                        let _ = this.render_row(1, window, cx);
                        let first = &this.veils[&SharedString::from("reply#body.0")];
                        let spans =
                            first
                                .borrow_mut()
                                .advance(0, "café histórico y nuevo", Instant::now());
                        assert_eq!(spans.len(), 1);
                        assert_eq!(
                            spans[0].0,
                            "café histórico".len().."café histórico y nuevo".len()
                        );
                        let second = &this.veils[&SharedString::from("reply#body.1")];
                        let spans = second
                            .borrow_mut()
                            .advance(1, "Otro párrafo", Instant::now());
                        assert_eq!(spans.len(), 1);
                        assert_eq!(spans[0].0, 0.."Otro párrafo".len());
                    });
                })
                .unwrap();
                let veil = transcript.read(cx).veils[&SharedString::from("reply#body.0")].clone();
                // A second history batch in another entry must not restart
                // the fade of this already visible live suffix.
                let recovered = assistant(
                    "other",
                    MessageStatus::Complete,
                    vec![text_part("body", "otra respuesta histórica")],
                );
                let mut next = live.clone();
                next.push(recovered.clone());
                let mut next_history = history.clone();
                next_history.push(recovered);
                transcript.update(cx, |this, cx| {
                    this.state.update(cx, |state, cx| {
                        state
                            .receive_transcript_update(
                                slopty_view_doc::TranscriptUpdate {
                                    frame: slopty_view_doc::diff_transcript(&live, &next),
                                    replay_baseline: Some(slopty_view_doc::TranscriptBaseline::capture(
                                        &next_history,
                                    )),
                                },
                                cx,
                            )
                            .unwrap();
                    });
                    this.sync(cx);
                    assert!(Rc::ptr_eq(
                        &veil,
                        &this.veils[&SharedString::from("reply#body.0")]
                    ));
                });
            });
        }

        #[test]
        fn text_streamed_under_reduced_motion_does_not_fade_in_when_motion_resumes() {
            fn render_rows(
                transcript: &Entity<Transcript>,
                window: gpui::WindowHandle<CachedTranscript>,
                cx: &mut gpui::App,
            ) {
                cx.update_window(window.into(), |_, window, cx| {
                    transcript.update(cx, |this, cx| {
                        for ix in 0..this.list.item_count() {
                            let _ = this.render_row(ix, window, cx);
                        }
                    });
                })
                .unwrap();
            }
            fn streaming(text: &str) -> Vec<SessionMessageEntry> {
                vec![
                    prompt("ask"),
                    assistant(
                        "reply",
                        MessageStatus::Streaming,
                        vec![text_part("body", text)],
                    ),
                ]
            }
            with_window(|transcript, window, cx| {
                let body = SharedString::from("reply#body.0");
                // Attach on the prompt alone, so the reply is live text rather
                // than an attach-time baseline.
                transcript.update(cx, |this, cx| feed(this, vec![prompt("ask")], cx));
                render_rows(&transcript, window, cx);

                // A row that first streams while motion is reduced.
                cx.set_reduce_motion(true);
                transcript.update(cx, |this, cx| feed(this, streaming("uno"), cx));
                render_rows(&transcript, window, cx);
                assert!(!transcript.read(cx).veils.contains_key(&body));
                cx.set_reduce_motion(false);
                render_rows(&transcript, window, cx);
                let veil = transcript.read(cx).veils[&body].clone();
                assert!(
                    veil.borrow_mut()
                        .advance(0, "uno", Instant::now())
                        .is_empty(),
                    "text painted under reduced motion must not dissolve in"
                );

                // A row already fading when motion pauses, then resumes.
                cx.set_reduce_motion(true);
                transcript.update(cx, |this, cx| feed(this, streaming("uno dos"), cx));
                render_rows(&transcript, window, cx);
                cx.set_reduce_motion(false);
                render_rows(&transcript, window, cx);
                let veil = transcript.read(cx).veils[&body].clone();
                assert!(
                    veil.borrow_mut()
                        .advance(0, "uno dos", Instant::now())
                        .is_empty(),
                    "text streamed while paused is already on screen"
                );
                let spans = veil.borrow_mut().advance(0, "uno dos tres", Instant::now());
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].0, "uno dos".len().."uno dos tres".len());
            });
        }


        fn feed(
            this: &mut Transcript,
            entries: Vec<SessionMessageEntry>,
            cx: &mut Context<Transcript>,
        ) {
            this.state.update(cx, |state, _| {
                state.selected_chat = Some("chat".into());
                state.transcript_replayed = true;
                state.transcript = entries;
                state.transcript_revision += 1;
            });
            this.sync(cx);
        }

        fn prompt(id: &str) -> SessionMessageEntry {
            let mut entry = assistant(
                id,
                MessageStatus::Complete,
                vec![text_part("text", "Please explain this.")],
            );
            entry.role = MessageRole::User;
            entry
        }

        fn tick(
            transcript: &Entity<Transcript>,
            window: gpui::WindowHandle<CachedTranscript>,
            cx: &mut gpui::App,
        ) {
            // slopty: the list follows its end in its own layout; a frame is all a tick is.
            transcript.update(cx, |_, cx| cx.notify());
            draw(window, cx);
        }

        fn wheel(window: gpui::WindowHandle<CachedTranscript>, delta: f32, cx: &mut gpui::App) {
            cx.update_window(window.into(), |_, window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::Scroll(gpui::ScrollEvent {
                        position: gpui::point(px(500.0), px(400.0)),
                        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(delta))),
                        ..Default::default()
                    }),
                    cx,
                );
            })
            .unwrap();
        }

        #[test]
        fn selecting_live_tail_keeps_anchor_visible_during_a_stream_burst() {
            with_window(|transcript, window, cx| {
                let text = (0..40)
                    .map(|i| format!("Paragraph {i} has selectable response text.\n\n"))
                    .collect::<String>();
                transcript.update(cx, |this, cx| {
                    feed(
                        this,
                        vec![assistant(
                            "reply",
                            MessageStatus::Streaming,
                            vec![text_part("text", &text)],
                        )],
                        cx,
                    );
                });
                draw(window, cx);
                let bounds = render::selection_test_bounds("reply#text.39:39");
                let start = bounds.origin + gpui::point(px(1.0), px(8.0));
                cx.update_window(window.into(), |_, window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                            button: MouseButton::Left,
                            position: start,
                            click_count: 1,
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .unwrap();
                assert!(crate::markdown::selection::is_dragging());
                assert!(
                    !transcript.read(cx).pinned,
                    "text mouse-down must release following"
                );
                assert!(
                    !transcript.read(cx).is_glued(),
                    "selection must materialize the viewport"
                );
                transcript.update(cx, |this, cx| {
                    feed(
                        this,
                        vec![assistant(
                            "reply",
                            MessageStatus::Streaming,
                            vec![text_part("text", &(text.clone() + &text))],
                        )],
                        cx,
                    );
                });
                draw(window, cx);
                cx.update_window(window.into(), |_, window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                            position: start + gpui::point(px(90.0), px(0.0)),
                            pressed_button: Some(MouseButton::Left),
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .unwrap();
                assert!(
                    crate::markdown::selection::selected_text().is_some(),
                    "streaming must not virtualize the drag anchor before the first move"
                );
                crate::markdown::selection::end_active_drag();
                crate::markdown::selection::clear_if_owner("reply#text.39:39");
            });
        }

        #[test]
        fn active_reply_text_selection_survives_streaming_and_completion() {
            with_window(|transcript, window, cx| {
                transcript.update(cx, |this, cx| {
                    feed(this, vec![prompt("prompt")], cx);
                    this.state.update(cx, |state, _| {
                        state.sessions.push(slopty_view_proto::Session {
                            last_completed_turn: None,
                            running_subagents: 0,
                            chat_id: "chat".into(),
                            device_id: "test".into(),
                            status: slopty_view_proto::SessionStatus::Working,
                            started_at: Some(chrono::Utc::now()),
                            updated_at: chrono::Utc::now(),
                            status_since: None, // slopty: new field.
                            waiting_on: None, // slopty: new field.
                        })
                    });
                });
                let entries = |status, text: &str| {
                    vec![
                        prompt("prompt"),
                        assistant("reply", status, vec![text_part("text", text)]),
                    ]
                };
                transcript.update(cx, |this, cx| {
                    feed(
                        this,
                        entries(MessageStatus::Streaming, "Selectable response text."),
                        cx,
                    )
                });
                for _ in 0..80 {
                    tick(&transcript, window, cx);
                }
                let bounds = render::selection_test_bounds("reply#text.0:0");
                let start = bounds.origin + gpui::point(px(1.0), px(8.0));
                let end = start + gpui::point(px(90.0), px(0.0));
                cx.update_window(window.into(), |_, window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                            button: MouseButton::Left,
                            position: start,
                            click_count: 1,
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .unwrap();
                assert!(crate::markdown::selection::is_dragging());
                draw(window, cx);
                cx.update_window(window.into(), |_, window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                            position: end,
                            pressed_button: Some(MouseButton::Left),
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .unwrap();
                let selected =
                    crate::markdown::selection::selected_text().expect("active text must select");
                assert!(!selected.is_empty());
                transcript.update(cx, |this, cx| {
                    feed(
                        this,
                        entries(
                            MessageStatus::Streaming,
                            "Selectable response text. More output.",
                        ),
                        cx,
                    )
                });
                draw(window, cx);
                assert_eq!(
                    crate::markdown::selection::selected_text(),
                    Some(selected.clone())
                );
                cx.update_window(window.into(), |_, window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                            button: MouseButton::Left,
                            position: end,
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .unwrap();
                transcript.update(cx, |this, cx| {
                    feed(
                        this,
                        entries(
                            MessageStatus::Complete,
                            "Selectable response text. More output.",
                        ),
                        cx,
                    )
                });
                draw(window, cx);
                assert_eq!(crate::markdown::selection::selected_text(), Some(selected));
                crate::markdown::selection::clear_if_owner("reply#text.0:0");
            });
        }







        // slopty: a jump reaches the end though rows measured under its glide
        // move the distance between wheel steps; a step up still breaks it.
        #[test]
        fn a_jump_reaches_the_end_whatever_rows_measure_under_it() {
            with_window(|transcript, window, cx| {
                transcript.update(cx, |this, cx| {
                    let entries = (0..12)
                        .map(|ix| {
                            assistant(
                                &format!("reply-{ix}"),
                                MessageStatus::Complete,
                                vec![text_part("text", &"A long paragraph.\n\n".repeat(20))],
                            )
                        })
                        .collect();
                    feed(this, entries, cx);
                });
                draw(window, cx);
                for _ in 0..6 {
                    wheel(window, 2_000.0, cx);
                    draw(window, cx);
                }
                assert!(transcript.read(cx).jump_button_shown());
                transcript.update(cx, |this, cx| this.jump_to_bottom(cx));
                tick(&transcript, window, cx);
                // Rows measured taller since the last observation, then the
                // wheel moves a little down: the pin holds.
                transcript.update(cx, |this, _| this.last_scroll_distance = 0.0);
                wheel(window, -5.0, cx);
                draw(window, cx);
                assert!(transcript.read(cx).pinned, "measured rows broke the jump");
                for _ in 0..120 {
                    tick(&transcript, window, cx);
                }
                let this = transcript.read(cx);
                assert!(this.distance_from_bottom() <= FOLLOW_PX, "the jump stopped short");
                assert!(!this.jump_button_shown());
                // A step up is the person leaving the end.
                wheel(window, 300.0, cx);
                draw(window, cx);
                assert!(!transcript.read(cx).pinned);
            });
        }






        /// Run the next queued diagram synchronously, as the worker would.
        fn render_next_diagram(transcript: &Entity<Transcript>, cx: &mut gpui::App) -> bool {
            transcript.update(cx, |this, cx| {
                let Some(code) = this.diagrams.borrow_mut().next_job() else {
                    return false;
                };
                let palette = crate::markdown::mermaid::Palette::from_theme(Theme::of(cx));
                let result = crate::markdown::mermaid::render(&code, &palette).and_then(|svg| {
                    crate::image_media::decode_image("image/svg+xml", svg.into_bytes())
                });
                assert!(result.is_ok(), "fixture diagram must render");
                this.finish_diagram(code, crate::theme::style_generation(), result, cx);
                true
            })
        }

        const MERMAID_FENCE: &str =
            "```mermaid\nflowchart TD\n    A[Request] --> B[Plan]\n    B --> C[Done]\n```\n";

        #[test]
        fn mermaid_streaming_tail_keeps_source_until_the_reply_moves_past_it() {
            with_window(|transcript, window, cx| {
                let reply = |status, text: String| {
                    vec![
                        prompt("prompt"),
                        assistant("reply", status, vec![text_part("text", &text)]),
                    ]
                };
                let mut text = String::from("Intro.\n\n");
                // Token-sized commits through the open fence, then the closed
                // fence while it is still the reply's tail.
                for token in MERMAID_FENCE.split_inclusive([' ', '\n']) {
                    text.push_str(token);
                    transcript.update(cx, |this, cx| {
                        feed(this, reply(MessageStatus::Streaming, text.clone()), cx)
                    });
                    tick(&transcript, window, cx);
                    let this = transcript.read(cx);
                    assert!(
                        this.diagram_worker.is_none(),
                        "a streaming tail fence must not start a render"
                    );
                    assert!(this.diagrams.borrow_mut().next_job().is_none());
                }
                // Enough reply after the fence that the viewport can anchor on
                // it without clamping back to the end.
                text.push_str(&format!(
                    "\nAfter the diagram. {}",
                    "More detail. ".repeat(300)
                ));
                transcript.update(cx, |this, cx| {
                    feed(this, reply(MessageStatus::Streaming, text.clone()), cx)
                });
                draw(window, cx);
                assert!(
                    transcript.read(cx).diagram_worker.is_some(),
                    "a fence followed by more reply renders"
                );
                // A pinned end is glued, where rows report no bounds; read
                // them with the viewport anchored at the diagram row instead.
                transcript.update(cx, |this, cx| {
                    this.pinned = false;
                    let ix = this
                        .rows
                        .iter()
                        .position(|row| row.id.as_ref() == "reply#text.1")
                        .unwrap();
                    this.list.scroll_to(ListOffset {
                        item_ix: ix,
                        offset_in_item: px(0.0),
                    });
                    cx.notify();
                });
                draw(window, cx);
                let fence_height = |cx: &gpui::App| {
                    let this = transcript.read(cx);
                    let ix = this
                        .rows
                        .iter()
                        .position(|row| row.id.as_ref() == "reply#text.1")
                        .unwrap();
                    this.list.bounds_for_item(ix).unwrap().size.height
                };
                let source_height = fence_height(cx);
                assert!(render_next_diagram(&transcript, cx));
                assert!(
                    !render_next_diagram(&transcript, cx),
                    "one request per source"
                );
                draw(window, cx);
                assert_ne!(
                    fence_height(cx),
                    source_height,
                    "the diagram replaced its source"
                );
                let frame: SharedString = "reply#text.1-mermaid-1".into();
                transcript.update(cx, |this, cx| {
                    assert_eq!(this.diagrams.borrow().ready_count(), 1);
                    this.diagrams.borrow_mut().toggle_source(&frame);
                    this.diagram_layout_changed(&["reply#text.1".into()], cx);
                });
                draw(window, cx);
                assert_eq!(
                    fence_height(cx),
                    source_height,
                    "the toggle restores the source"
                );
                // Completion keeps row identity, the rendered diagram and
                // the reader's source toggle; nothing renders again.
                transcript.update(cx, |this, cx| {
                    feed(this, reply(MessageStatus::Complete, text.clone()), cx)
                });
                draw(window, cx);
                let this = transcript.read(cx);
                assert!(this.diagrams.borrow().source_visible(&frame));
                assert!(this.diagrams.borrow_mut().next_job().is_none());
            });
        }

        #[test]
        fn mermaid_diagram_swap_keeps_a_pinned_stream_at_the_bottom() {
            with_window(|transcript, window, cx| {
                let intro = "A paragraph that explains the result in detail. ".repeat(12);
                let reply = |status, tail: &str| {
                    let mut text = String::new();
                    for section in 0..8 {
                        text.push_str(&format!("Section {section}. {intro}\n\n"));
                    }
                    text.push_str(MERMAID_FENCE);
                    text.push_str(tail);
                    vec![
                        prompt("prompt"),
                        assistant("reply", status, vec![text_part("text", &text)]),
                    ]
                };
                transcript.update(cx, |this, cx| {
                    feed(
                        this,
                        reply(MessageStatus::Streaming, "\nAfter the diagram."),
                        cx,
                    )
                });
                for _ in 0..50 {
                    tick(&transcript, window, cx);
                }
                let this = transcript.read(cx);
                assert!(this.pinned);
                assert!(
                    this.list.max_offset_for_scrollbar().y > px(100.0),
                    "must overflow"
                );
                assert!(this.distance_from_bottom() <= 1.0);
                assert!(render_next_diagram(&transcript, cx));
                for _ in 0..50 {
                    tick(&transcript, window, cx);
                }
                let this = transcript.read(cx);
                assert!(this.pinned, "a diagram swap is not user input");
                assert!(this.distance_from_bottom() <= 1.0, "pin follows the swap");
                // More streaming after the swap keeps following.
                transcript.update(cx, |this, cx| {
                    feed(
                        this,
                        reply(
                            MessageStatus::Streaming,
                            &format!("\nAfter the diagram.\n\n{intro}\n\n{intro}"),
                        ),
                        cx,
                    )
                });
                for _ in 0..50 {
                    tick(&transcript, window, cx);
                }
                let this = transcript.read(cx);
                assert!(this.pinned);
                assert!(this.distance_from_bottom() <= 1.0);
                assert!(this.diagrams.borrow_mut().next_job().is_none());
            });
        }







        #[test]
        fn navigation_cancels_fold_compensation_before_queued_frame() {
            with_transcript(|transcript, cx| {
                for navigation in ["wheel", "rail", "bottom"] {
                    transcript.user_collapse_scroll = Some(UserCollapseScroll {
                        started_at: Instant::now(),
                        duration_ms: 850,
                        height_delta: 2000.0,
                        row_ix: 0,
                        initial_top: -1000.0,
                        target_top: 80.0,
                    });
                    let asked = transcript.user_collapse_scroll_scheduled.ask(Instant::now());
                    let hold_token = transcript.user_hold_token;
                    match navigation {
                        "wheel" => transcript.handle_scroll(
                            &ListScrollEvent {
                                visible_range: 0..0,
                                count: 0,
                                is_scrolled: true,
                                is_following_tail: false,
                            },
                            cx,
                        ),
                        "rail" => transcript.begin_scroll_navigation(),
                        "bottom" => transcript.jump_to_bottom(cx),
                        _ => unreachable!(),
                    }
                    assert!(transcript.user_collapse_scroll.is_none(), "{navigation}");
                    assert_ne!(
                        transcript.user_hold_token, hold_token,
                        "cancel stale long presses"
                    );
                    assert!(
                        transcript.user_collapse_scroll_scheduled.waiting(),
                        "keep the queued-frame guard"
                    );

                    // A frame queued before the input must neither move the
                    // viewport nor resurrect the canceled compensation.
                    let offset = transcript.list.logical_scroll_top();
                    assert!(transcript.user_collapse_scroll_scheduled.answered(asked.unwrap()));
                    transcript.step_user_collapse_scroll(cx);
                    let after = transcript.list.logical_scroll_top();
                    assert_eq!(after.item_ix, offset.item_ix);
                    assert_eq!(after.offset_in_item, offset.offset_in_item);
                    assert!(transcript.user_collapse_scroll.is_none());
                }
            });
        }
    }

    /// Explicit multiline and long soft-wrapped prompts get a fold affordance;
    /// short messages stay untouched.
    #[test]
    fn long_prompts_collapse_and_short_ones_do_not() {
        assert!(!user_message_needs_collapse("short message"));
        assert!(!user_message_needs_collapse("1\n2\n3\n4\n5"));
        assert!(user_message_needs_collapse("1\n2\n3\n4\n5\n6"));
        assert!(
            !user_message_needs_collapse(&"x".repeat(240)),
            "ordinary two- or three-line prose must not grow a toggle"
        );
        assert!(!user_message_needs_collapse(
            &"x".repeat(USER_COLLAPSE_CHARS)
        ));
        assert!(user_message_needs_collapse(
            &"x".repeat(USER_COLLAPSE_CHARS + 1)
        ));
    }

    #[test]
    fn user_resize_duration_scales_with_distance_and_stays_bounded() {
        let short = user_resize_duration_ms(100.0);
        let medium = user_resize_duration_ms(600.0);
        let long = user_resize_duration_ms(2_000.0);
        assert!((220..=260).contains(&short));
        assert!(medium > short);
        assert_eq!(long, 850);
        assert_eq!(
            user_resize_spec(100.0).curve,
            motion::EASE_OUT,
            "short folds keep the decisive sidebar-like ease-out"
        );
        assert_eq!(
            user_resize_spec(2_000.0).curve,
            motion::EASE_IN_OUT,
            "large folds avoid front-loading the whole travel"
        );
    }

    /// The toggle is render-local: expanding a prompt must not change the
    /// row's identity or version, or the list would splice (and the
    /// virtualizer would drop the scroll anchor) on every click.
    #[test]
    fn expanding_a_prompt_is_not_a_row_change() {
        let mut entry = assistant("u3", MessageStatus::Complete, vec![]);
        entry.role = MessageRole::User;
        entry.status = None;
        entry.parts = vec![text_part("t0", &"a line\n".repeat(40))];
        let before = rows_for_entry(&entry, false, &mut parse);
        let after = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].id, after[0].id);
        assert_eq!(before[0].version, after[0].version);
    }

    #[test]
    fn prompts_written_in_markdown_render_it() {
        let markdown = |text: &str| {
            let mut entry = assistant("u1", MessageStatus::Complete, vec![text_part("t0", text)]);
            entry.role = MessageRole::User; // slopty: a prompt, as the record has it
            entry.status = None;
            let rows = rows_for_entry(&entry, false, &mut parse);
            let RowKind::User { markdown, .. } = &rows[0].kind else {
                panic!("expected a user row");
            };
            markdown.clone()
        };
        let tree = markdown("Keep **this**\nand `that`\n\n- one\n- two")
            .unwrap()
            .full;
        assert_eq!(tree.blocks.len(), 2);
        // Typed newlines stay line breaks.
        let Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!("expected a paragraph");
        };
        assert!(runs.iter().any(|run| run.text.contains('\n')));
        // Prose reads the same as typed, so it keeps the plain bubble.
        assert!(markdown("just a question\n\nand another").is_none());
    }

    #[test]
    fn long_prompts_fold_to_their_leading_markdown() {
        let markdown = |text: &str| {
            let mut entry = assistant("u1", MessageStatus::Complete, vec![text_part("t0", text)]);
            entry.role = MessageRole::User; // slopty: a prompt, as the record has it
            entry.status = None;
            let rows = rows_for_entry(&entry, false, &mut parse);
            let RowKind::User { markdown, .. } = &rows[0].kind else {
                panic!("expected a user row");
            };
            markdown.clone().unwrap()
        };
        let lines = USER_COLLAPSED_LINES + USER_FOLD_OVERDRAW_LINES;
        // A prompt that fits whole folds to itself.
        assert!(markdown("**short**\n\n- one\n- two").folded.is_none());
        // A long report keeps its heading and the items that fill the fold.
        let report: String = (1..=240).map(|i| format!("{i}. **case {i}**\n")).collect();
        let report = markdown(&format!("## Sweep\n\n{report}"));
        let folded = report.folded.unwrap();
        assert_eq!(folded.blocks.len(), 2);
        let Block::List { items, .. } = &folded.blocks[1].block else {
            panic!("expected the list");
        };
        assert_eq!(items.len(), lines - 1);
        let Block::List { items: all, .. } = &report.full.blocks[1].block else {
            panic!("expected the list");
        };
        // A prefix: each kept item is the item it was.
        assert_eq!(items[..], all[..items.len()]);
        // A long fence keeps its first lines; blocks after the fold go.
        let fence: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let fenced = markdown(&format!("Run:\n\n```\n{fence}```\n\nthen **this**"));
        let folded = fenced.folded.unwrap();
        assert_eq!(folded.blocks.len(), 2);
        let Block::CodeBlock { code, .. } = &folded.blocks[1].block else {
            panic!("expected the fence");
        };
        assert_eq!(code.lines().count(), lines - 2);
        assert!(code.starts_with("line 1\nline 2"));
    }

    #[test]
    fn user_rows_split_attachment_refs_from_text() {
        let content = crate::attachments::with_attachments(
            "what color is this?",
            &["/data/uploads/ab12-red.png".to_string()],
        );
        let mut entry = assistant("u2", MessageStatus::Complete, vec![]);
        entry.role = MessageRole::User;
        entry.status = None;
        entry.parts = vec![text_part("t0", &content)];
        let rows = rows_for_entry(&entry, false, &mut parse);
        assert_eq!(rows.len(), 1);
        let RowKind::User {
            text, attachments, ..
        } = &rows[0].kind
        else {
            panic!("expected a user row");
        };
        assert_eq!(text.as_ref(), "what color is this?");
        assert_eq!(rows[0].copy_text.as_deref(), Some("what color is this?"));
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].path, "/data/uploads/ab12-red.png");
        assert_eq!(attachments[0].name, "ab12-red.png");

        // Image-only send: no bubble text, refs parsed.
        let only = crate::attachments::with_attachments("", &["/a/p.png".to_string()]);
        entry.parts = vec![text_part("t0", &only)];
        let rows = rows_for_entry(&entry, false, &mut parse);
        let RowKind::User {
            text, attachments, ..
        } = &rows[0].kind
        else {
            panic!("expected a user row");
        };
        assert_eq!(text.as_ref(), "");
        assert!(rows[0].copy_text.is_none());
        assert_eq!(attachments.len(), 1);
    }

    #[test]
    fn diff_rows_appends_and_middle_edits() {
        let entry1 = assistant("m1", MessageStatus::Complete, vec![text_part("t0", "one")]);
        let entry2 = assistant("m2", MessageStatus::Complete, vec![text_part("t0", "two")]);
        let r1 = rows_for_entry(&entry1, false, &mut parse);
        let mut both = r1.clone();
        both.extend(rows_for_entry(&entry2, false, &mut parse));

        // Identical → None.
        assert!(diff_rows(&r1, &r1.clone()).is_none());
        // Append → splice at the tail.
        assert_eq!(diff_rows(&r1, &both), Some((1..1, 1)));
        // Removal from the end.
        assert_eq!(diff_rows(&both, &r1), Some((1..2, 0)));

        // Middle content change: only the changed row splices.
        let entry1b = assistant(
            "m1",
            MessageStatus::Complete,
            vec![text_part("t0", "one more")],
        );
        let mut both_b = rows_for_entry(&entry1b, false, &mut parse);
        both_b.extend(rows_for_entry(&entry2, false, &mut parse));
        assert_eq!(diff_rows(&both, &both_b), Some((0..1, 1)));

        // Full reset when everything shifts.
        let r2 = rows_for_entry(&entry2, false, &mut parse);
        assert_eq!(diff_rows(&r1, &r2), Some((0..1, 1)));
    }

    #[test]
    fn diff_handles_live_to_split_growth() {
        let live = assistant("m1", MessageStatus::Streaming, vec![text_part("t0", MD)]);
        let done = assistant("m1", MessageStatus::Complete, vec![text_part("t0", MD)]);
        let live_rows = rows_for_entry(&live, false, &mut parse);
        let done_rows = rows_for_entry(&done, false, &mut parse);
        // Same ids; every version flips its streaming bit → one 3-row splice.
        assert_eq!(diff_rows(&live_rows, &done_rows), Some((0..3, 3)));
    }

    #[test]
    fn tool_diff_builds_real_hunks_with_context_and_numbers() {
        use crate::changes::LineKind;
        let old = (1..=20).map(|i| format!("line {i}")).collect::<Vec<_>>();
        let mut new = old.clone();
        new[9] = "LINE 10".into();
        let diff = slopty_view_proto::ToolDiff {
            path: "/w/a.rs".into(),
            old_text: Some(old.join("\n") + "\n"),
            new_text: new.join("\n") + "\n",
        };
        let Some(ToolDetail::Diff {
            file,
            old_text,
            new_text,
        }) = tool_detail(None, Some(&diff), None)
        else {
            panic!("expected diff detail");
        };
        // One hunk: the change plus 3 context lines each side, real numbers.
        assert_eq!(file.hunks.len(), 1);
        let hunk = &file.hunks[0];
        assert_eq!(hunk.header, "@@ -7,7 +7,7 @@");
        assert_eq!(hunk.lines.len(), 8); // 6 context + 1 del + 1 add
        let del = hunk
            .lines
            .iter()
            .find(|l| l.kind == LineKind::Del)
            .expect("del line");
        assert_eq!(del.old_no, Some(10));
        assert_eq!(del.new_no, None);
        assert_eq!(del.text, "line 10");
        let add = hunk
            .lines
            .iter()
            .find(|l| l.kind == LineKind::Add)
            .expect("add line");
        assert_eq!(add.new_no, Some(10));
        assert_eq!(add.text, "LINE 10");
        assert_eq!((file.additions, file.deletions), (1, 1));
        assert_eq!(old_text.as_deref(), diff.old_text.as_deref());
        assert_eq!(new_text.as_deref(), Some(diff.new_text.as_str()));
        // New files carry Added status (and no old numbers).
        let created = slopty_view_proto::ToolDiff {
            path: "/w/new.txt".into(),
            old_text: None,
            new_text: "only\n".into(),
        };
        let Some(ToolDetail::Diff {
            file,
            old_text,
            new_text,
        }) = tool_detail(None, Some(&created), None)
        else {
            panic!("expected diff detail");
        };
        assert_eq!(file.status, crate::changes::FileStatus::Added);
        assert!(old_text.is_none());
        assert_eq!(new_text.as_deref(), Some("only\n"));

        // Output: verbatim lines (indentation intact), counted-tail cap.
        let output = (0..40)
            .map(|i| format!("    indented {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let Some(ToolDetail::Output {
            lines,
            truncated_by,
            ..
        }) = tool_detail(Some(&output), None, None)
        else {
            panic!("expected output detail");
        };
        assert_eq!(lines.len(), OUTPUT_DETAIL_MAX_LINES);
        assert_eq!(truncated_by, 40 - OUTPUT_DETAIL_MAX_LINES);
        assert_eq!(lines[0].as_ref(), "    indented 0");

        // A minified megabyte line is cut on a char boundary before shaping,
        // in the summary and in a fetched full output alike.
        let long = format!("{}\nshort", "é".repeat(1024 * 1024));
        for detail in [
            tool_detail(Some(&long), None, None),
            blob_detail(&long, false),
        ] {
            let Some(ToolDetail::Output { lines, .. }) = detail else {
                panic!("expected output detail");
            };
            assert_eq!(lines[0].chars().count(), OUTPUT_LINE_MAX_CHARS + 1);
            assert!(lines[0].ends_with("é…"));
            assert_eq!(lines[1].as_ref(), "short");
        }

        // slopty: a line of JSON reads pretty-printed, in the summary and the whole output alike.
        let json = r#"[{"name":"studio","liveness":"online"}]"#;
        for detail in [tool_detail(Some(json), None, None), blob_detail(json, false)] {
            let Some(ToolDetail::Output { lines, .. }) = detail else {
                panic!("expected output detail");
            };
            assert!(lines.len() > 1, "pretty-printed: {lines:?}");
            assert!(lines.iter().any(|line| line.trim() == r#""liveness": "online""#));
        }

        // Nothing → no affordance.
        assert!(tool_detail(None, None, None).is_none());
        assert!(tool_detail(Some("\n\n"), None, None).is_none());
    }

    #[test]
    fn tool_group_summaries() {
        let exec = |c: &str| ToolItem {
            part_id: "fixture".into(),
            call: ToolCall::Exec { command: c.into() },
            is_error: false,
            resolved: true,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            kind: ToolItemKind::Call,
            images: Arc::new([]),
            facts: None,
        };
        let edit = |p: &str| ToolItem {
            part_id: "fixture".into(),
            call: ToolCall::EditFile {
                path: p.into(),
                old_string: None,
                new_string: None,
            },
            is_error: false,
            resolved: true,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            kind: ToolItemKind::Call,
            images: Arc::new([]),
            facts: None,
        };
        let tools = vec![
            // slopty: a listing counts apart ("listed 1 directory"); a command here.
            exec("git status"),
            exec("pwd"),
            exec("make"),
            edit("a.rs"),
            edit("b.rs"),
        ];
        assert_eq!(
            tool_group_summary(&tools),
            "Ran 3 commands and edited 2 files"
        );
        // slopty: the TUI's parts past the shared summary: an edit's line totals, directories
        // listed, and what commands landed, each once.
        let with = |mut item: ToolItem, facts: slopty_view_doc::ToolFacts| {
            item.facts = Some(Arc::new(facts));
            item
        };
        let changed = |added, removed| slopty_view_doc::ToolFacts {
            changed: Some([added, removed]),
            ..slopty_view_doc::ToolFacts::default()
        };
        let landed = |words: &str| slopty_view_doc::ToolFacts {
            landed: Some(words.to_owned()),
            ..slopty_view_doc::ToolFacts::default()
        };
        let tools = vec![
            with(edit("a.rs"), changed(10, 2)),
            with(edit("b.rs"), changed(2, 1)),
            exec("ls src"),
            with(exec("git commit -m x"), landed("committed 1a2b3c4")),
            with(exec("git push"), landed("committed 1a2b3c4 · pushed to main")),
        ];
        assert_eq!(
            tool_group_summary(&tools),
            "Ran 2 commands, edited 2 files (+12 −3), listed 1 directory, committed 1a2b3c4, \
             pushed to main"
        );
        // Distinct-path dedupe: editing one file twice counts once.
        let tools = vec![edit("a.rs"), edit("a.rs")];
        assert_eq!(tool_group_summary(&tools), "Edited 1 file");
        // A failure is said in its own clause's parentheses.
        let mut failing = exec("boom");
        failing.is_error = true;
        assert_eq!(tool_group_summary(&[failing]), "Ran 1 command (1 failed)");
        // slopty: a failed listing listed nothing: a failed command, said with the commands.
        let mut gone = exec("ls missing");
        gone.is_error = true;
        assert_eq!(tool_group_summary(&[exec("ls src"), exec("make"), gone]), "Ran 2 commands (1 failed) and listed 1 directory");
        // slopty: the person's own Deny is no failure.
        let mut denied = exec("rm -rf build");
        denied.is_error = true;
        denied.facts = Some(std::sync::Arc::new(slopty_view_doc::ToolFacts {
            rejected: true,
            ..slopty_view_doc::ToolFacts::default()
        }));
        assert_eq!(tool_group_summary(&[denied]), "Ran 1 command");
        // Reads / searches / misc.
        let tools = vec![
            ToolItem {
                part_id: "fixture".into(),
                call: ToolCall::ReadFile { path: "x".into() },
                is_error: false,
                resolved: true,
                detail: None,
                invocation: None,
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                subagent_ref: None,
                subagent_status: None,
                subagent_tail: None,
                kind: ToolItemKind::Call,
                images: Arc::new([]),
                facts: None,
            },
            ToolItem {
                part_id: "fixture".into(),
                call: ToolCall::Glob {
                    pattern: "*.rs".into(),
                },
                is_error: false,
                resolved: true,
                detail: None,
                invocation: None,
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                subagent_ref: None,
                subagent_status: None,
                subagent_tail: None,
                kind: ToolItemKind::Call,
                images: Arc::new([]),
                facts: None,
            },
            ToolItem {
                part_id: "fixture".into(),
                call: ToolCall::WebSearch { query: "q".into() },
                is_error: false,
                resolved: true,
                detail: None,
                invocation: None,
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                subagent_ref: None,
                subagent_status: None,
                subagent_tail: None,
                kind: ToolItemKind::Call,
                images: Arc::new([]),
                facts: None,
            },
        ];
        assert_eq!(tool_group_summary(&tools), "Read 1 file and searched 2 times");
        // slopty: a subagent's run is said as the work handed off, not as a tool called.
        let spawn = ToolItem {
            call: ToolCall::Unknown {
                name: "Agent".into(),
                input: Some(serde_json::json!({ "description": "Look" })),
            },
            ..exec("unused")
        };
        assert_eq!(
            tool_group_summary(&[exec("make"), spawn.clone()]),
            "Ran 1 command and delegated to 1 subagent"
        );
        let running = ToolItem { resolved: false, ..spawn };
        assert_eq!(
            tool_group_summary_waiting(&[running], None, false).text.as_ref(),
            "Running 1 subagent"
        );
    }

    #[test]
    fn subagent_tab_titles() {
        // The tab is the BARE task — the "Agent:" genus is stripped.
        let named = ToolCall::Unknown {
            name: "Agent: scan repo".into(),
            input: None,
        };
        assert_eq!(subagent_tab_title(&named).as_ref(), "scan repo");
        // A bare "Task"/"Agent" digs the description out of the call input
        // (which sheds any genus of its own).
        let bare = ToolCall::Unknown {
            name: "Task".into(),
            input: Some(serde_json::json!({
                "description": "Agent: audit the auth flow",
                "prompt": "very long instructions…",
            })),
        };
        assert_eq!(subagent_tab_title(&bare).as_ref(), "audit the auth flow");
        // Word boundaries only — a name that merely STARTS with the genus
        // keeps itself.
        let compound = ToolCall::Unknown {
            name: "Taskmaster".into(),
            input: None,
        };
        assert_eq!(subagent_tab_title(&compound).as_ref(), "Taskmaster");
        // Nothing to derive → the generic label.
        let blank = ToolCall::Unknown {
            name: "agent".into(),
            input: None,
        };
        assert_eq!(subagent_tab_title(&blank).as_ref(), "Subagent");
        // Absurd lengths cap with an ellipsis; multiline prompts keep only
        // their first line.
        let long = ToolCall::Unknown {
            name: "x".repeat(120),
            input: None,
        };
        let title = subagent_tab_title(&long);
        assert_eq!(title.chars().count(), SUBAGENT_TITLE_MAX + 1);
        assert!(title.ends_with('…'));
        // Non-spawn-shaped calls stay generic.
        assert_eq!(
            subagent_tab_title(&ToolCall::Exec {
                command: "ls".into()
            })
            .as_ref(),
            "Subagent"
        );
    }

    #[test]
    fn tool_chip_labels_per_kind() {
        assert_eq!(
            tool_chip_content(&ToolCall::Exec {
                command: "cargo test".into()
            }),
            ("Run", "cargo test".to_string())
        );
        assert_eq!(
            tool_chip_content(&ToolCall::Search {
                pattern: "foo".into(),
                path: Some("src".into())
            }),
            ("Search", "foo in src".to_string())
        );
        assert_eq!(
            tool_chip_content(&ToolCall::ApplyPatch { path: None }),
            ("Patch", "workspace".to_string())
        );
        assert_eq!(
            tool_chip_content(&ToolCall::Mcp {
                server: "gh".into(),
                tool: "issues".into(),
                input: None
            }),
            ("MCP", "gh · issues".to_string())
        );
        let todo = ToolCall::Todo {
            items: vec![
                slopty_view_proto::TodoItem::new("a", slopty_view_proto::TodoStatus::Completed),
                slopty_view_proto::TodoItem::new("b", slopty_view_proto::TodoStatus::Pending),
            ],
        };
        assert_eq!(tool_chip_content(&todo), ("Todo", "1/2 done".to_string()));
    }

    #[test]
    fn file_action_badges_show_only_the_file_name() {
        assert_eq!(file_badge_name("/Users/me/project/src/main.rs"), "main.rs");
        assert_eq!(
            file_badge_name("crates/ui/src/transcript.rs"),
            "transcript.rs"
        );
        assert_eq!(file_badge_name(r"C:\project\src\main.rs"), "main.rs");
        assert_eq!(file_badge_name("src/components/"), "components");
        assert_eq!(file_badge_name("main.rs"), "main.rs");
        assert_eq!(file_badge_name(""), "");
    }

    #[gpui::test]
    fn tool_file_badge_opens_linkable_files_without_toggling_the_chip(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::Modifiers;

        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let opened: Rc<RefCell<Vec<render::LinkActivation>>> = Rc::default();
        let recorded = opened.clone();
        let opener = ToolFileOpener {
            link: render::LinkUi {
                source_session: Some("chat".into()),
                source_local: true,
                file_roots: Some(Rc::new(vec![crate::workspace_links::FileLinkRoot {
                    chat: Some("chat".into()),
                    root: "/work/app".into(),
                    local: true,
                }])),
                handler: Rc::new(move |activation, _, _| {
                    recorded.borrow_mut().push(activation.clone());
                    render::LinkOutcome::Internal
                }),
            },
        };
        assert!(opener.opens("/work/app/src/guard.rs"));
        assert!(opener.opens("src/guard.rs"));
        // An absolute file outside every root still opens, read-only.
        assert!(opener.opens("/tmp/report.md"));
        assert!(!opener.opens("/work/app/../secret.rs"));
        assert!(!opener.opens("../secret.rs"));

        let read = |path: &str| ToolItem {
            part_id: "fixture".into(),
            images: Default::default(),
            facts: None,
            call: ToolCall::ReadFile { path: path.into() },
            is_error: false,
            resolved: true,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            kind: ToolItemKind::Call,
        };
        // The chip header inside a clickable parent, standing in for the
        // accordion toggle that wraps it in `render_tool_group`.
        struct ChipFixture {
            tool: ToolItem,
            opener: ToolFileOpener,
            toggles: Rc<Cell<usize>>,
        }
        impl Render for ChipFixture {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = Theme::of(cx).clone();
                let toggles = self.toggles.clone();
                div().w(px(600.0)).child(
                    div()
                        .id("chip-toggle")
                        .debug_selector(|| "chip-toggle".into())
                        .on_click(move |_, _, _| toggles.set(toggles.get() + 1))
                        .child(chip_header(
                            &self.tool,
                            false,
                            1.0,
                            Some(&self.opener),
                            None,
                            None,
                            &theme,
                            cx.entity_id(),
                            cx,
                        )),
                )
            }
        }

        let toggles = Rc::new(Cell::new(0));
        let (_inside, window) = cx.add_window_view(|_, _| ChipFixture {
            tool: read("/work/app/src/guard.rs"),
            opener: opener.clone(),
            toggles: toggles.clone(),
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        let badge = window
            .debug_bounds("tool-file-badge")
            .expect("a workspace file badge is clickable");
        window.simulate_click(badge.center(), Modifiers::default());
        {
            let opened = opened.borrow();
            assert_eq!(opened.len(), 1);
            assert_eq!(opened[0].target.original, "/work/app/src/guard.rs");
            assert_eq!(opened[0].action, render::LinkAction::Primary);
            assert_eq!(opened[0].source_session.as_deref(), Some("chat"));
        }
        assert_eq!(
            toggles.get(),
            0,
            "opening the file must not toggle the chip"
        );
        // The rest of the header still toggles the accordion.
        let row = window.debug_bounds("chip-toggle").unwrap();
        window.simulate_click(
            gpui::point(row.left() + px(2.0), row.center().y),
            Modifiers::default(),
        );
        assert_eq!(toggles.get(), 1);
        assert_eq!(opened.borrow().len(), 1);

        let unlinkable_toggles = Rc::new(Cell::new(0));
        let (_unlinkable, window) = cx.add_window_view(|_, _| ChipFixture {
            tool: read("../secret.rs"),
            opener: opener.clone(),
            toggles: unlinkable_toggles.clone(),
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            window.debug_bounds("tool-file-badge").is_none(),
            "paths the link handler cannot open keep an inert badge"
        );
    }

    #[test]
    fn multiline_command_flattens_to_one_chip_line() {
        // The user's breaker: a multi-line script in a Run chip. The detail
        // must come out as ONE sanitized line — the chip's fixed 30px card
        // then truncates it with an ellipsis like the original's CSS.
        let (label, detail) = tool_chip_content(&ToolCall::Exec {
            command: "set -e\nfixture_in_original=0\n\tgrep -c  \"x\"".into(),
        });
        assert_eq!(label, "Run");
        assert_eq!(detail, "set -e fixture_in_original=0 grep -c \"x\"");
        assert!(!detail.contains('\n'));
        // The chip row height is a constant, independent of content shape.
        assert_eq!(chips_height(1), CHIPS_TOP_PAD + CHIP_HEIGHT);
        // Every detail kind is sanitized (MCP inputs / queries are model text).
        let (_, q) = tool_chip_content(&ToolCall::WebSearch {
            query: "line one\nline two".into(),
        });
        assert_eq!(q, "line one line two");
    }

    #[test]
    fn call_block_carries_the_full_invocation() {
        // Multi-line command: verbatim lines, not the flattened chip line.
        let Some(ToolDetail::Output {
            lines,
            truncated_by,
            ..
        }) = call_block(&ToolCall::Exec {
            command: "set -e\ncargo test".into(),
        })
        else {
            panic!("expected an output block")
        };
        assert_eq!(truncated_by, 0);
        assert_eq!(
            lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
            vec!["set -e", "cargo test"]
        );

        // A long single-line command soft-wraps instead of ellipsizing.
        let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Exec {
            command: "x".repeat(CALL_WRAP_COLS * 2 + 10),
        }) else {
            panic!("expected an output block")
        };
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.chars().count() <= CALL_WRAP_COLS));

        // MCP input pretty-prints under the `server · tool` line.
        let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Mcp {
            server: "gh".into(),
            tool: "issues".into(),
            input: Some(serde_json::json!({"repo": "slopty"})),
        }) else {
            panic!("expected an output block")
        };
        assert_eq!(lines[0].as_ref(), "gh · issues");
        assert!(lines.iter().any(|l| l.contains("\"repo\": \"slopty\"")));

        // Todos list one item per line with checkbox state.
        let Some(ToolDetail::Output { lines, .. }) = call_block(&ToolCall::Todo {
            items: vec![
                slopty_view_proto::TodoItem::new("a", slopty_view_proto::TodoStatus::Completed),
                slopty_view_proto::TodoItem::new("b", slopty_view_proto::TodoStatus::InProgress),
                slopty_view_proto::TodoItem::new("c", slopty_view_proto::TodoStatus::Pending),
            ],
        }) else {
            panic!("expected an output block")
        };
        assert_eq!(
            lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
            vec!["[x] a", "[~] b", "[ ] c"]
        );

        // Blank invocation → no block; the chip stays a plain card.
        assert!(
            call_block(&ToolCall::Exec {
                command: "  \n ".into()
            })
            .is_none()
        );
    }

    #[test]
    fn timestamp_strip_lands_on_the_last_settled_row() {
        use chrono::FixedOffset;
        // Fixed zone (UTC−4). slopty: the time as near as it is (#17): alone today,
        // "Yesterday" the day before, the date this year, the year before it.
        let tz = FixedOffset::west_opt(4 * 3600).unwrap();
        let at = |rfc: &str| chrono::DateTime::parse_from_rfc3339(rfc).unwrap().timestamp_millis();
        let ms = at("2026-07-01T19:45:00Z");
        // On the system's clock.
        let clock = if clock_format().contains('p') { "3:45 PM" } else { "15:45" };
        assert_eq!(format_timestamp_near(ms, at("2026-07-01T23:00:00Z"), &tz), clock);
        assert_eq!(
            format_timestamp_near(ms, at("2026-07-02T10:00:00Z"), &tz),
            format!("Yesterday {clock}")
        );
        assert_eq!(
            format_timestamp_near(ms, at("2026-09-02T10:00:00Z"), &tz),
            format!("Jul 1, {clock}")
        );
        assert_eq!(format_timestamp_near(ms, at("2027-01-02T10:00:00Z"), &tz), "Jul 1, 2026");
        assert_eq!(format_full_timestamp(ms, &tz), format!("Wednesday, Jul 1, 2026, {clock}"));

        // User entries carry the strip on their single row (pending too).
        let user = SessionMessageEntry {
            id: "u1".into(),
            role: MessageRole::User,
            parts: vec![text_part("p1", "hi")],
            created_at: ms,
            device_id: "dev".into(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        };
        let rows = rows_for_entry(&user, true, &mut parse);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].timestamp, Some(ms));

        // Assistant entries: strip on the LAST row once settled…
        let done = assistant(
            "a1",
            MessageStatus::Complete,
            vec![text_part("p1", "one\n\ntwo")],
        );
        let rows = rows_for_entry(&done, false, &mut parse);
        assert!(rows.len() >= 2);
        assert_eq!(rows.last().unwrap().timestamp, Some(done.created_at));
        assert_eq!(
            rows.last().unwrap().copy_text.as_deref(),
            Some("one\n\ntwo")
        );
        assert!(rows[..rows.len() - 1].iter().all(|r| r.timestamp.is_none()));
        assert!(rows[..rows.len() - 1].iter().all(|r| r.copy_text.is_none()));

        // …but never mid-stream (chat-view.tsx: no hover under a moving reply).
        let live = assistant(
            "a2",
            MessageStatus::Streaming,
            vec![text_part("p1", "streaming…")],
        );
        let rows = rows_for_entry(&live, false, &mut parse);
        assert!(rows.iter().all(|r| r.timestamp.is_none()));
        assert!(rows.iter().all(|r| r.copy_text.is_none()));
        // Every row knows its entry (the hover group).
        assert!(rows.iter().all(|r| r.entry_id.as_ref() == live.id));
    }

    #[test]
    fn message_copy_keeps_authored_text_and_excludes_tool_traces() {
        let entry = assistant(
            "a-copy",
            MessageStatus::Complete,
            vec![
                text_part("p1", "First **paragraph**."),
                tool_part("tool", "printf hidden"),
                text_part("p2", "    indented code\n    stays indented"),
            ],
        );
        assert_eq!(
            assistant_copy_text(&entry).as_deref(),
            Some("First **paragraph**.\n\n    indented code\n    stays indented")
        );
    }

    #[test]
    fn single_line_collapses_all_whitespace_runs() {
        assert_eq!(single_line("a\nb"), "a b");
        assert_eq!(single_line("  a\t\t b \r\n c  "), "a b c");
        assert_eq!(single_line("plain"), "plain");
        assert_eq!(single_line(""), "");
        assert_eq!(single_line("\n\n"), "");
    }

    /// slopty: a dialog's chip names it a confirm and shows its ask; an agent's question keeps
    /// its header; an approval shows its command, not its generic header (#8).
    #[test]
    fn an_ask_chip_names_its_kind_and_shows_the_ask() {
        let dialog = slopty_view_proto::UserInputQuestion {
            kind: "dialog:remote_callout".into(),
            header: "Confirm".into(),
            question: "Enable Remote Control?\n\nContinue from your phone.".into(),
            ..Default::default()
        };
        let (label, value) = input_chip_words(&[dialog]);
        assert_eq!((label.as_ref(), value.as_ref()), ("Confirm", "Enable Remote Control?"));
        let asked = slopty_view_proto::UserInputQuestion {
            kind: "question".into(),
            header: "Scope".into(),
            question: "Which tests?".into(),
            ..Default::default()
        };
        let (label, value) = input_chip_words(&[asked]);
        assert_eq!((label.as_ref(), value.as_ref()), ("Question", "Scope"));
        let approval = slopty_view_proto::UserInputQuestion {
            kind: "approval".into(),
            header: "Run cargo test?".into(),
            ..Default::default()
        };
        assert_eq!(input_chip_words(&[approval]).1.as_ref(), "Run cargo test?");
        // The view model carries the command apart; the words only describe it.
        let bash = slopty_view_proto::UserInputQuestion {
            kind: "approval".into(),
            header: "Bash command".into(),
            question: "Create refused.txt file".into(),
            command: Some("touch refused.txt".into()),
            ..Default::default()
        };
        assert_eq!(input_chip_words(&[bash]), ("Approval".into(), "touch refused.txt".into()));
        let bare = slopty_view_proto::UserInputQuestion {
            kind: "approval".into(),
            header: "Bash command".into(),
            command: Some("rm -rf build".into()),
            ..Default::default()
        };
        assert_eq!(input_chip_words(&[bare]).1.as_ref(), "rm -rf build", "never \"Run a command\"");
    }

    #[test]
    fn chips_height_is_analytic() {
        assert_eq!(chips_height(0), 0.0);
        assert_eq!(chips_height(1), CHIPS_TOP_PAD + CHIP_HEIGHT);
        assert_eq!(
            chips_height(3),
            CHIPS_TOP_PAD + 3.0 * CHIP_HEIGHT + 2.0 * CHIP_GAP
        );
    }

    #[test]
    fn tool_row_reveal_honors_delay_easing_and_reduced_motion() {
        let epoch = Instant::now();
        let start = epoch + Duration::from_millis(TOOL_ROW_STAGGER_MS);
        assert_eq!(tool_row_reveal_progress(Some(start), epoch, false), 0.0);
        let halfway = tool_row_reveal_progress(
            Some(start),
            start + Duration::from_millis(TOOL_ROW_REVEAL.duration_ms / 2),
            false,
        );
        assert!(halfway > 0.5 && halfway < 1.0);
        assert_eq!(
            tool_row_reveal_progress(Some(start), start + TOOL_ROW_REVEAL.total(), false,),
            1.0
        );
        assert_eq!(tool_row_reveal_progress(Some(start), epoch, true), 1.0);
        assert_eq!(tool_row_reveal_progress(None, epoch, false), 1.0);
    }

    #[test]
    fn tool_connector_draws_continuously_before_revealing_the_branch() {
        let epoch = Instant::now();
        let start = epoch + Duration::from_millis(TOOL_ROW_STAGGER_MS);
        assert_eq!(
            tool_connector_reveal_progress(Some(start), epoch, false),
            0.0
        );
        assert_eq!(
            tool_connector_reveal_progress(Some(start), epoch, true),
            1.0
        );
        assert_eq!(tool_connector_reveal_progress(None, epoch, false), 1.0);
        let halfway = tool_connector_reveal_progress(
            Some(start),
            start + Duration::from_millis(TOOL_CONNECTOR_REVEAL.duration_ms / 2),
            false,
        );
        assert!(halfway > 0.9 && halfway < 1.0);

        assert_eq!(tool_connector_parts(0.0, false), (0.0, 0.0));
        assert_eq!(tool_connector_parts(0.44, true), (0.0, 0.0));
        assert_eq!(tool_connector_continuation(Some(0.0)), 0.0);
        assert!(tool_connector_continuation(Some(0.3)) > 0.0);
        assert_eq!(tool_connector_continuation(Some(0.45)), 1.0);

        let (incoming, branch) = tool_connector_parts(0.60, true);
        assert!(incoming > 0.0 && incoming < 1.0);
        assert_eq!(branch, 0.0);
        assert_eq!(tool_connector_parts(1.0, true), (1.0, 1.0));
        assert_eq!(tool_connector_continuation(None), 0.0);
    }

    #[test]
    fn tool_branch_reveal_tracks_distance_through_the_bend() {
        let length = |points: &[Point<f32>]| -> f32 {
            points
                .windows(2)
                .map(|p| (p[1].x - p[0].x).hypot(p[1].y - p[0].y))
                .sum()
        };
        let full = activity_branch_points(1.0);
        assert_eq!(activity_branch_points(0.0), vec![point(0.0, 0.0)]);
        let end = full.last().unwrap();
        assert!((end.x - (ACTIVITY_BRANCH_END_X - ACTIVITY_TRUNK_X)).abs() < 0.0001);
        assert!((end.y - ACTIVITY_BEND_RADIUS).abs() < 0.0001);
        for progress in [0.1, 0.25, 0.5, 0.75, 0.9] {
            let partial = activity_branch_points(progress);
            assert!((length(&partial) / length(&full) - progress).abs() < 0.0001);
            assert!(
                partial
                    .windows(2)
                    .all(|p| p[1].x >= p[0].x && p[1].y >= p[0].y)
            );
        }
    }

    /// slopty: a title under way shimmers on the one clock every shimmer runs on: glyph by glyph
    /// from faint through the accent to its crest as the band crosses it (opencode's
    /// `text-shimmer`, HextaUI's accent crest).
    #[test]
    fn a_title_under_way_shimmers_glyph_by_glyph_on_the_shared_clock() {
        let theme = Theme::dark();
        let moment = Instant::now();
        let cycle = Duration::from_secs_f32(motion::SHIMMER_PERIOD_MS / 1000.0);
        assert_eq!(shimmer_ms(moment), motion::locked_phase(cycle, moment) * motion::SHIMMER_PERIOD_MS);
        // Half the band's pass past glyph 0: glyph 0 at full ink, glyphs far behind still faint.
        let crest = motion::SHIMMER_BAND * motion::SHIMMER_STEP_MS / 2.0;
        let runs = title_shimmer_runs("Running 2 commands", crest, &theme);
        assert_eq!(runs.len(), "Running 2 commands".len(), "one run a glyph");
        assert_eq!(runs[0].1.color, Some(motion::shimmer_color(1.0, &theme)), "glyph 0 is at the crest");
        assert_eq!(runs[12].1.color, Some(theme.text_faint), "ahead of the band, faint");
        let between = runs[2].1.color.expect("inked");
        assert!(between != runs[0].1.color.expect("inked") && between != theme.text_faint, "the band's shoulder is between");
    }

    /// slopty: a message's time and actions fade in over 200 ms as the pointer arrives, and out
    /// as it leaves, from wherever they stood (the Amp study's item 9); still, they snap.
    #[test]
    fn a_message_strip_fades_in_and_out() {
        let mut fades = HashMap::new();
        let key: SharedString = "row".into();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(strip_fade(&mut fades, &key, false, start, false), (0.0, false), "unseen at rest");
        assert_eq!(strip_fade(&mut fades, &key, true, start, false), (0.0, true), "arriving");
        let (half, moving) = strip_fade(&mut fades, &key, true, at(100), false);
        assert!(moving && half > 0.3 && half < 1.0, "{half}");
        assert_eq!(strip_fade(&mut fades, &key, true, at(200), false), (1.0, false));
        let (leaving, _) = strip_fade(&mut fades, &key, false, at(300), false);
        assert_eq!(leaving, 1.0, "leaves from where it stood");
        assert_eq!(strip_fade(&mut fades, &key, false, at(500), false), (0.0, false));
        assert!(fades.is_empty(), "a gone strip is forgotten");
        assert_eq!(strip_fade(&mut fades, &key, true, at(600), true), (1.0, false), "still, it snaps");
    }

    #[test]
    fn flavour_words_rotate_every_seven_seconds() {
        let seed = flavour_seed("chat-1");
        assert_eq!(flavour_word(seed, 0), flavour_word(seed, 6));
        assert_ne!(flavour_word(seed, 0), flavour_word(seed, 7));
        // Deterministic per chat; different chats usually differ in phase.
        assert_eq!(flavour_word(seed, 3), flavour_word(seed, 3));
    }

    #[test]
    fn elapsed_format_scales_from_seconds_to_days() {
        for (secs, expected) in [
            (-5, "0s"),
            (0, "0s"),
            (59, "59s"),
            (60, "1m"),
            (92, "1m 32s"),
            (310, "5m 10s"),
            (3_599, "59m 59s"),
            (3_600, "1h"),
            (4_800, "1h 20m"),
            (6_000, "1h 40m"),
            (86_399, "23h 59m"),
            (86_400, "1d"),
            (183_845, "2d 3h"),
        ] {
            assert_eq!(format_elapsed(secs), expected, "elapsed seconds: {secs}");
        }
    }

    /// slopty: a live timer pads its second unit, so it holds its width as it counts.
    #[test]
    fn a_ticking_timer_pads_its_second_unit() {
        assert_eq!(ticking_elapsed(9), "9s");
        assert_eq!(ticking_elapsed(65), "1m 05s");
        assert_eq!(ticking_elapsed(3_900), "1h 05m");
        assert_eq!(ticking_elapsed(90_000), "1d 01h");
    }

    #[test]
    fn empty_text_parts_produce_no_rows() {
        let entry = assistant(
            "m9",
            MessageStatus::Streaming,
            vec![text_part("t0", ""), text_part("t1", "   ")],
        );
        assert!(rows_for_entry(&entry, false, &mut parse).is_empty());
    }
}

// slopty: appshots removed.
