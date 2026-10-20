// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! The right-pane "Changes" content (feature-inventory §1.11): a unified-diff
//! viewer over `WatchCheckoutDiffs`.
//!
//! - pure patch parser: `diff --git` sections → file/hunk/line/notice rows,
//!   with add/delete/rename/binary detection and per-file counts;
//! - resolution: the shown diff matches the selected chat by `checkout_id`
//!   first, then by device+cwd, then cwd alone;
//! - states: *preparing* (no diff yet), *clean* (empty patch), *list*; a watch
//!   error shows a banner while the last content stays;
//! - virtualized with gpui `list()` at LINE granularity — every file header,
//!   unchanged-lines row, and diff line is its own row (the flat model Zed's editor
//!   uses for its project diff: only the visible slice materializes, and a
//!   collapsed file's body rows are removed from the list outright, not
//!   hidden); nowrap sections fold (`motion::fold_frame`) on a
//!   clipped stand-in row (analytic heights, capped to what the clip can
//!   reveal), while variable-height wrapped sections settle immediately;
//! - syntax highlight reuses the markdown tokenizer per diff line, computed
//!   time-sliced on the background executor and applied as paint-only run
//!   colors (layout never changes);
//! - scopes (t3code parity): *Working tree* rides the watch stream; *Branch
//!   changes* (vs a selectable base ref, default branch preselected) and
//!   *Latest turn* fetch one-shot `GetCheckoutDiff` captures, refreshed when
//!   the watch checksum says the tree moved;
//! - two layouts ([`DiffMode`], toolbar toggle, persisted): *unified* stacks
//!   old and new; *split* pairs each hunk's deletions against its additions
//!   into one row with two columns. Split is a pure re-flatten of the same
//!   parse — the row model, virtualization, folds, and highlights are shared.
//!   Its left column is inert: notes are cited against the post-change file,
//!   so only the right column takes a `+` (already-staged old-side notes
//!   still show their cards).
//! - long-line wrapping is a persisted toolbar choice. It only changes the
//!   code plane: gutters, comment affordances, and sticky headers stay fixed,
//!   while the virtual list measures each logical row's resulting height.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, ListAlignment, ListState,
    SharedString, Subscription, Task, Window, div, font, list, prelude::*, px,
};
use unicode_width::UnicodeWidthChar as _;

use slopty_view_proto::{Chat, CheckoutDiff}; // slopty: no History surface (ruled out)
use slopty_view_rpc::methods;

use crate::comments::{self, CommentSide, ReviewComment};
use crate::composer::{ComposerInput, ComposerInputEvent};

use crate::markdown::render;
use crate::motion::{self, AnimationExt as _, CHEVRON};
use crate::popover::{self, Popup};
use crate::settings::widgets::ActionHint as _;
use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;
use slopty_syntax::LanguageId as Lang;
use crate::motion::HoverFade as _;

// slopty: find in the diff, the whitespace filter, the changed files' tree and any turn's diff.
mod aids;
mod review;
mod words;
pub use review::{DiffMatch, TreeRow, TurnChoice, diff_tree, find_matches, turn_choices};
use review::scoped_params;

// slopty: the diff's find, and a transcript's "Open diff" landing on a turn. The shell binds the
// keys, and on `ShowTurnDiff` reveals a diff tab, which takes the turn [`show_turn`] asked for.
gpui::actions!(
    changes,
    [FindInDiff, FindNextInDiff, FindPreviousInDiff, ShowTurnDiff, ToggleDiffTree]
);

// slopty: ⇧⌘Y shows or hides the changed files' tree (Claude desktop's `toggleDiffFileList`).
/// Binds the pane's own keys that the shell's keymap does not.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new(
        &crate::settings::platform_combo("mod-shift-y"),
        ToggleDiffTree,
        Some("Changes"),
    )]);
}

// slopty: the turn a transcript asked to see the diff of, until a diff tab takes it.
#[derive(Default)]
struct RequestedTurn(Option<String>);

impl gpui::Global for RequestedTurn {}

/// Open the diff at the turn of entry `turn` (the person's message that began it, or any entry of
/// its answer): a diff tab takes it, and the shell is asked to show one ([`ShowTurnDiff`]).
pub(crate) fn show_turn(turn: &str, window: &mut Window, cx: &mut App) {
    cx.set_global(RequestedTurn(Some(turn.to_owned())));
    window.dispatch_action(Box::new(ShowTurnDiff), cx);
}

// ---------------------------------------------------------------------------
// Layout numbers (analytic — they drive the fold tween)
// ---------------------------------------------------------------------------

pub const FILE_HEADER_HEIGHT: f32 = crate::surface_chrome::HEADER_HEIGHT;
const STICKY_FILE_HEADER_BLUR: f32 = 16.0;
/// Coverage of the theme's content-plane tint over the sticky header blur.
/// Light needs substantially more coverage: dark text is much more vulnerable
/// to rows ghosting through the blur than light text is on a dark tint.
const STICKY_FILE_HEADER_TINT_ALPHA_DARK: f32 = 0.40;
const STICKY_FILE_HEADER_TINT_ALPHA_LIGHT: f32 = 0.85;
/// slopty: the "N unchanged lines" row between hunks, a line's height plus air (opencode's
/// separator is one 24 px line; item 11).
pub const UNCHANGED_HEIGHT: f32 = 24.0;
/// slopty: how many unchanged lines one click on their row shows, nearest the change below
/// (opencode's `expansionLineCount`); ⌥ shows them all.
const UNCHANGED_STEP: u32 = 20;
// slopty: the pane's one code line (`files::preview`'s), 20 px at the default code size.
pub const DIFF_LINE_HEIGHT: f32 = 20.0;
pub const NOTICE_HEIGHT: f32 = 24.0;
pub const BODY_BOTTOM_PAD: f32 = 8.0;
/// One line-number digit at the gutter's 11 px mono.
const GUTTER_DIGIT: f32 = 6.6;
/// A gutter's 8 px right pad and the 6 px gap that keeps its number off the bar.
const GUTTER_PAD: f32 = 14.0;
/// The digits every gutter has room for. slopty: three, so a file under a thousand lines never
/// widens it (opencode's 3 ch number column; item 11).
const GUTTER_MIN_DIGITS: u32 = 3;
/// Gutter width per line-number column at its floor; wider numbers widen it by [`gutter_width`].
pub const GUTTER_WIDTH: f32 = GUTTER_MIN_DIGITS as f32 * GUTTER_DIGIT + GUTTER_PAD;
/// Width of the bar on the left edge of +/− rows. slopty: the bar alone marks an added or
/// deleted line, 2 px in its full hue, with no +/− column (opencode's `diffIndicators: "bars"`;
/// item 11).
pub const ACCENT_BAR_WIDTH: f32 = 2.0;
/// Hairline between the two split columns.
pub const SPLIT_DIVIDER_WIDTH: f32 = 1.0;
const DIFF_TEXT_SIZE: f32 = crate::typography::CODE_FONT_SIZE_DEFAULT; // slopty: one code size
const DIFF_TAB_SIZE: usize = 4;
/// slopty: the band a diff line fades over where the pane cuts it.
const DIFF_EDGE_FADE: f32 = 16.0;
/// Size of the painted diff body text, and the size the column measurement in
/// [`DiffHorizontalGeometry::resolve`] must use: they desync otherwise. It is the
/// `code_font_size` setting itself, as the editor's and a read-only file's.
pub(crate) fn diff_text_size(theme: &Theme) -> f32 {
    crate::typography::clamp_font_size(theme.code_font_size)
}

/// The row box and the painted line box must agree, or code clips once the
/// user moves the code font size off [`DIFF_TEXT_SIZE`].
pub(crate) fn diff_line_height(theme: &Theme) -> f32 {
    diff_text_size(theme) * (DIFF_LINE_HEIGHT / DIFF_TEXT_SIZE)
}

pub(crate) const UNIFIED_CODE_PADDING_LEFT: f32 = 12.0;
pub(crate) const SPLIT_CODE_PADDING_LEFT: f32 = 6.0;
/// Breathing room after the widest source line when scrolled fully right.
const CODE_PADDING_RIGHT: f32 = 24.0;

/// How the diff is laid out. Persisted in `ui-settings.json` (`diffSplit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    /// One column: deletions above additions (the classic patch reading).
    #[default]
    Unified,
    /// Two columns: old on the left, new on the right, paired per hunk.
    Split,
}

impl DiffMode {
    pub fn from_split(split: bool) -> Self {
        if split { Self::Split } else { Self::Unified }
    }

    pub fn is_split(self) -> bool {
        self == Self::Split
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Unified => Self::Split,
            Self::Split => Self::Unified,
        }
    }
}

// ---------------------------------------------------------------------------
// Patch model + parser (pure)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    /// `\ No newline at end of file` and friends.
    Meta,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceSide {
    Old,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceLineRef {
    pub side: SourceSide,
    /// One-based source line number.
    pub line_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffHighlights {
    pub old: Option<Arc<slopty_syntax::HighlightedDocument>>,
    pub new: Option<Arc<slopty_syntax::HighlightedDocument>>,
}

impl DiffHighlights {
    pub fn source_ref(&self, line: &DiffLine) -> Option<SourceLineRef> {
        match line.kind {
            LineKind::Del => line.old_no.map(|line_number| SourceLineRef {
                side: SourceSide::Old,
                line_number,
            }),
            LineKind::Add => line.new_no.map(|line_number| SourceLineRef {
                side: SourceSide::New,
                line_number,
            }),
            LineKind::Context => line
                .new_no
                .filter(|_| self.new.is_some())
                .map(|line_number| SourceLineRef {
                    side: SourceSide::New,
                    line_number,
                })
                .or_else(|| {
                    line.old_no.map(|line_number| SourceLineRef {
                        side: SourceSide::Old,
                        line_number,
                    })
                }),
            LineKind::Meta => None,
        }
    }

    pub fn spans(&self, line: &DiffLine) -> &[slopty_syntax::HighlightSpan] {
        let Some(source_ref) = self.source_ref(line) else {
            return &[];
        };
        let document = match source_ref.side {
            SourceSide::Old => self.old.as_deref(),
            SourceSide::New => self.new.as_deref(),
        };
        document
            .and_then(|document| document.lines.get(source_ref.line_number as usize - 1))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    /// Display path (the post-change side).
    pub path: String,
    /// Pre-rename path, when different.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    /// Parser-collected notices (mode changes etc.).
    pub notices: Vec<String>,
    pub hunks: Vec<Hunk>,
    pub additions: u32,
    pub deletions: u32,
    /// Largest line number on either side — sizes the gutters analytically
    /// (a fixed column overflowed past 4 digits; user report).
    pub max_line: u32,
}

impl FileDiff {
    fn new(path: String, old_path: Option<String>) -> Self {
        Self {
            path,
            old_path,
            status: FileStatus::Modified,
            binary: false,
            notices: Vec::new(),
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            max_line: 0,
        }
    }
}

/// Width of one line-number gutter column, fitted to the file's largest
/// line number: 11px mono ≈ 6.6px per digit, the 8px right pad, and a 6px
/// left gap so the number never abuts the accent bar (at 4 digits the old
/// formula left 1.6px — visually touching; user report). Never narrower
/// than the [`GUTTER_WIDTH`] floor.
pub fn gutter_width(file: &FileDiff) -> f32 {
    let digits = (file.max_line.max(1).ilog10() + 1).max(GUTTER_MIN_DIGITS);
    digits as f32 * GUTTER_DIGIT + GUTTER_PAD
}

/// Width inputs that are independent of the active window's font metrics.
/// They are computed once with the parsed patch, off the render path.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DiffHorizontalGeometry {
    max_code_columns: usize,
    max_gutter_width: f32,
}

impl DiffHorizontalGeometry {
    fn from_file(file: &FileDiff) -> Self {
        let max_code_columns = file
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .map(|line| visual_columns(&line.text))
            .max()
            .unwrap_or(0);
        let max_gutter_width = gutter_width(file);
        Self {
            max_code_columns,
            max_gutter_width,
        }
    }
}

/// Measure the same runs the row paints. Color boundaries can break kerning
/// and ligatures on native platforms even when every run uses the same font.
fn max_shaped_text_width(
    file: &FileDiff,
    highlights: Option<&DiffHighlights>,
    theme: &Theme,
    text_system: &gpui::WindowTextSystem,
) -> f32 {
    let mono = font(theme.font_mono.clone());
    let size = px(diff_text_size(theme));
    let column_width = text_system
        .ch_advance(text_system.resolve_font(&mono), size)
        .unwrap_or(size * 0.6)
        .as_f32();
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .fold(0.0f32, |widest, line| {
            let runs = line_runs(line, highlights, theme);
            let shaped = text_system
                .shape_line(line.text.clone().into(), size, &runs, None)
                .width()
                .as_f32();
            // Preserve the old column estimate as a floor, including tab stops.
            widest
                .max(shaped)
                .max(visual_columns(&line.text) as f32 * column_width)
        })
}

/// Count terminal-style display columns, including tab stops and wide
/// Unicode glyphs. This is only a floor; actual shaped runs determine the extent.
pub(crate) fn visual_columns(text: &str) -> usize {
    text.chars().fold(0usize, |columns, ch| {
        if ch == '\t' {
            columns + (DIFF_TAB_SIZE - columns % DIFF_TAB_SIZE)
        } else {
            columns + ch.width().unwrap_or(0)
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DiffHorizontalMetrics {
    max_text_width: f32,
    max_gutter_width: f32,
}

impl DiffHorizontalMetrics {
    /// Compensating for the file-local gutter keeps every unified code
    /// viewport's effective scroll range identical.
    fn unified_content_width(self, gutter_width: f32) -> f32 {
        self.max_text_width
            + UNIFIED_CODE_PADDING_LEFT
            + CODE_PADDING_RIGHT
            + 2.0 * (self.max_gutter_width - gutter_width)
    }

    /// Split has one gutter per half. Both halves use this same extent so old
    /// and new remain synchronized even when one side is a filler.
    fn split_content_width(self, gutter_width: f32) -> f32 {
        self.max_text_width
            + SPLIT_CODE_PADDING_LEFT
            + CODE_PADDING_RIGHT
            + (self.max_gutter_width - gutter_width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DiffCodeWidth {
    /// Inline tool diffs keep their existing local clipping behavior.
    Clipped,
    /// Changes rows expose a stable intrinsic code width.
    Scrollable(DiffHorizontalMetrics),
    /// Changes rows consume their viewport width and grow vertically.
    Wrapped,
}

#[derive(Clone)]
struct DiffCodeScroll {
    handle: gpui::ScrollHandle,
    id: SharedString,
}

#[derive(Clone)]
struct DiffCodeScrollContext {
    handle: gpui::ScrollHandle,
    prefix: SharedString,
}

impl DiffCodeScrollContext {
    fn slot(&self, suffix: impl std::fmt::Display) -> DiffCodeScroll {
        DiffCodeScroll {
            handle: self.handle.clone(),
            id: SharedString::from(format!("{}-{suffix}", self.prefix)),
        }
    }
}

fn reset_horizontal_scroll(handle: &gpui::ScrollHandle) {
    handle.set_offset(gpui::Point::default());
}

fn strip_git_prefix(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

/// Split the tail of a `diff --git a/… b/…` line into (old, new) paths.
/// Quoted paths (spaces/unicode) are handled; for unquoted paths with spaces
/// the split favors the last ` b/` separator, which is git's own convention.
fn parse_git_paths(rest: &str) -> (String, String) {
    fn unquote(s: &str) -> String {
        let trimmed = s.trim();
        if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
            trimmed[1..trimmed.len() - 1]
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
        } else {
            trimmed.to_string()
        }
    }
    if let Some(pos) = rest.rfind(" b/").or_else(|| rest.rfind(" \"b/")) {
        let old = unquote(&rest[..pos]);
        let new = unquote(&rest[pos + 1..]);
        (
            strip_git_prefix(&old).to_string(),
            strip_git_prefix(&new).to_string(),
        )
    } else {
        let p = strip_git_prefix(&unquote(rest)).to_string();
        (p.clone(), p)
    }
}

/// Parse one `@@ -a[,b] +c[,d] @@ …` header into starting line numbers.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@")?;
    let minus = rest.find('-')?;
    let after_minus = &rest[minus + 1..];
    let old: u32 = after_minus
        .split(|c: char| c == ',' || c.is_whitespace())
        .next()?
        .parse()
        .ok()?;
    let plus = rest.find('+')?;
    let after_plus = &rest[plus + 1..];
    let new: u32 = after_plus
        .split(|c: char| c == ',' || c.is_whitespace())
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

/// slopty: a hunk's first line and its line count on the old and the new side, from its
/// `@@ -a[,b] +c[,d] @@` header. A side with no lines names the line before the hunk, so its
/// first line is the one after that.
fn hunk_span(header: &str) -> Option<[(u32, u32); 2]> {
    let mut sides = header.strip_prefix("@@")?.split_whitespace();
    let mut side = |sign: char| -> Option<(u32, u32)> {
        let range = sides.next()?.strip_prefix(sign)?;
        let (start, count) = match range.split_once(',') {
            Some((start, count)) => (start.parse().ok()?, count.parse().ok()?),
            None => (range.parse().ok()?, 1),
        };
        Some((if count == 0 { start + 1 } else { start }, count))
    };
    Some([side('-')?, side('+')?])
}

/// slopty: how many unchanged lines sit between hunk `ix` and the one before it, or the top of
/// the file for the first: what its "N unchanged lines" row counts (item 11). Read on the new
/// side, so lines that differ only in whitespace count when whitespace is ignored.
pub(crate) fn unchanged_before(file: &FileDiff, ix: usize) -> u32 {
    let span = |ix: usize| file.hunks.get(ix).and_then(|hunk| hunk_span(&hunk.header));
    let Some([_, (first, _)]) = span(ix) else {
        return 0;
    };
    let after = match ix.checked_sub(1) {
        None => 1,
        Some(before) => match span(before) {
            Some([_, (start, count)]) => start + count,
            None => return 0,
        },
    };
    first.saturating_sub(after)
}

/// slopty: show up to `most` of the unchanged lines above hunk `ix`, the nearest first, taking
/// their text from `source`, the new side's lines whole: they join the hunk as context and its
/// header widens to match. False when there is nothing to show or `source` lacks a line.
pub(crate) fn reveal_unchanged(
    file: &mut FileDiff,
    ix: usize,
    most: u32,
    source: &[String],
) -> bool {
    let hidden = unchanged_before(file, ix);
    let Some(hunk) = file.hunks.get_mut(ix) else {
        return false;
    };
    let Some([(old_first, old_count), (new_first, new_count)]) = hunk_span(&hunk.header) else {
        return false;
    };
    let shown = hidden.min(most);
    if shown == 0 {
        return false;
    }
    let from = new_first - shown;
    let Some(texts) = source.get(from as usize - 1..new_first as usize - 1) else {
        return false;
    };
    let revealed = texts.iter().zip(from..).map(|(text, new_no)| DiffLine {
        kind: LineKind::Context,
        old_no: (new_no + old_first).checked_sub(new_first).filter(|no| *no > 0),
        new_no: Some(new_no),
        text: text.clone(),
    });
    hunk.lines.splice(0..0, revealed);
    let (_, context) = split_hunk_header(&hunk.header);
    hunk.header = format!(
        "@@ -{},{} +{},{} @@{}{context}",
        old_first.saturating_sub(shown).max(1),
        old_count + shown,
        from,
        new_count + shown,
        if context.is_empty() { "" } else { " " },
    );
    true
}

/// Parse a unified git patch into file sections. Tolerant: unknown header
/// lines are skipped, truncated hunks keep what parsed so far.
pub fn parse_patch(patch: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut in_hunk = false;
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;

    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            let (old, new) = parse_git_paths(rest);
            let old_path = (old != new).then_some(old);
            files.push(FileDiff::new(new, old_path));
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };

        if raw.starts_with("@@") {
            if let Some((o, n)) = parse_hunk_header(raw) {
                old_no = o;
                new_no = n;
                file.hunks.push(Hunk {
                    header: raw.to_string(),
                    lines: Vec::new(),
                });
                in_hunk = true;
            }
            continue;
        }

        if in_hunk {
            let mut chars = raw.chars();
            let marker = chars.next();
            let body: String = chars.collect();
            let line = match marker {
                Some('+') => {
                    file.additions += 1;
                    let l = DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(new_no),
                        text: body,
                    };
                    new_no += 1;
                    Some(l)
                }
                Some('-') => {
                    file.deletions += 1;
                    let l = DiffLine {
                        kind: LineKind::Del,
                        old_no: Some(old_no),
                        new_no: None,
                        text: body,
                    };
                    old_no += 1;
                    Some(l)
                }
                Some(' ') | None => {
                    let l = DiffLine {
                        kind: LineKind::Context,
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        text: body,
                    };
                    old_no += 1;
                    new_no += 1;
                    Some(l)
                }
                Some('\\') => Some(DiffLine {
                    kind: LineKind::Meta,
                    old_no: None,
                    new_no: None,
                    text: raw.trim_start_matches('\\').trim().to_string(),
                }),
                _ => {
                    // A non-hunk line ends the hunk; reprocess as a header.
                    in_hunk = false;
                    None
                }
            };
            if let Some(line) = line
                && let Some(hunk) = file.hunks.last_mut()
            {
                file.max_line = file
                    .max_line
                    .max(line.old_no.unwrap_or(0))
                    .max(line.new_no.unwrap_or(0));
                hunk.lines.push(line);
                continue;
            }
            if in_hunk {
                continue;
            }
        }

        // File header territory.
        if raw.starts_with("new file mode") {
            file.status = FileStatus::Added;
        } else if raw.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
        } else if let Some(from) = raw.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(from.trim().to_string());
        } else if let Some(to) = raw.strip_prefix("rename to ") {
            file.status = FileStatus::Renamed;
            file.path = to.trim().to_string();
        } else if raw.starts_with("Binary files") || raw.starts_with("GIT binary patch") {
            file.binary = true;
        } else if let Some(mode) = raw.strip_prefix("new mode ") {
            file.notices
                .push(format!("Mode changed to {}", mode.trim()));
        } else if let Some(new) = raw.strip_prefix("+++ ") {
            let new = new.trim();
            if new == "/dev/null" {
                file.status = FileStatus::Deleted;
            } else if file.old_path.is_none() {
                file.path = strip_git_prefix(new).to_string();
            }
        } else if let Some(old) = raw.strip_prefix("--- ")
            && old.trim() == "/dev/null"
        {
            file.status = FileStatus::Added;
        }
        // "index …", "similarity index …", "old mode …" etc.: skipped.
    }
    files
}

/// Derived per-file notice rows (new/deleted/renamed/binary + parser notices).
pub fn file_notices(file: &FileDiff) -> Vec<String> {
    let mut notices = Vec::new();
    match file.status {
        FileStatus::Added => notices.push("New file".to_string()),
        FileStatus::Deleted => notices.push("Deleted file".to_string()),
        FileStatus::Renamed => {
            let from = file.old_path.as_deref().unwrap_or("?");
            notices.push(format!("Renamed from {from}"));
        }
        FileStatus::Modified => {}
    }
    if file.binary {
        notices.push("Binary file — contents not shown".to_string());
    }
    notices.extend(file.notices.iter().cloned());
    notices
}

/// Cap a file's hunks at `max_lines` total diff lines, appending a notice
/// when lines were dropped. The transcript renders a tool diff as ONE
/// stacked element inside its row, so an unbounded diff (a fetched
/// full-diff blob, a whole-file rewrite) would otherwise build tens of
/// thousands of elements every frame it is visible.
pub fn truncate_file_lines(file: &mut FileDiff, max_lines: usize) {
    let total: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    if total <= max_lines {
        return;
    }
    let mut budget = max_lines;
    file.hunks.retain_mut(|hunk| {
        if budget == 0 {
            return false;
        }
        if hunk.lines.len() > budget {
            hunk.lines.truncate(budget);
        }
        budget -= hunk.lines.len();
        true
    });
    file.notices.push(format!(
        "Diff truncated — showing first {max_lines} of {total} lines"
    ));
    // The gutter fits what actually renders.
    file.max_line = file
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l| l.old_no.unwrap_or(0).max(l.new_no.unwrap_or(0)))
        .max()
        .unwrap_or(0);
}

// slopty: a large diff opens with its files collapsed past a budget, each loaded by a click on
// its header, as Claude desktop does ("Files are collapsed for large diffs"; `docs/decisions/
// ui.md`, "A large diff opens collapsed").
/// A file with more diff lines than this opens collapsed.
pub const LARGE_FILE_LINES: usize = 1_500;
/// The diff lines a diff opens with across its files: a file that would pass it opens collapsed.
pub const DIFF_LINE_BUDGET: usize = 4_000;

/// A file's diff lines, every hunk's.
pub fn diff_lines(file: &FileDiff) -> usize {
    file.hunks.iter().map(|hunk| hunk.lines.len()).sum()
}

/// Which of `files` open collapsed, in order: one past [`LARGE_FILE_LINES`], and one that would
/// take the lines the open files before it spent past [`DIFF_LINE_BUDGET`]. A smaller file after
/// a held one still opens while it fits.
pub fn held_back(files: &[FileDiff]) -> Vec<bool> {
    let mut spent = 0_usize;
    files
        .iter()
        .map(|file| {
            let lines = diff_lines(file);
            let held = lines > LARGE_FILE_LINES || spent.saturating_add(lines) > DIFF_LINE_BUDGET;
            if !held {
                spent = spent.saturating_add(lines);
            }
            held
        })
        .collect()
}

/// Analytic expanded-body height — drives the 180 ms fold tween without
/// measurement.
pub fn body_height(file: &FileDiff) -> f32 {
    body_height_with(file, &[], None, DiffMode::Unified, DIFF_LINE_HEIGHT)
}

pub fn body_height_with(
    file: &FileDiff,
    comments: &[ReviewComment],
    draft: Option<(CommentSide, u32)>,
    mode: DiffMode,
    line_h: f32,
) -> f32 {
    body_rows(0, file, comments, draft, mode)
        .iter()
        .map(|row| row.height(comments, line_h))
        .sum()
}

/// One split row: indices into the hunk's lines for the left (old) and right
/// (new) column. `None` on a side means that column is empty for this row.
pub type LinePair = (Option<u32>, Option<u32>);

/// Pair a hunk's lines into split rows.
///
/// A hunk reads as runs: context lines sit on both sides, and each run of
/// deletions immediately followed by additions is a *change block* whose two
/// sides line up index-for-index (the shape git already emits — an edited
/// line's `-`/`+` are adjacent). The longer side's leftovers get one-sided
/// rows, so a 3-for-1 rewrite is 1 paired row and 2 add-only rows rather than
/// a ragged interleave. A deletion arriving after additions opens a new block
/// (`-a +b -c +d` is two edits, not one four-line one).
///
/// Pure and index-only: the caller keeps owning the lines, and the result is
/// small enough to live in the row model.
pub fn split_pairs(lines: &[DiffLine]) -> Vec<LinePair> {
    split_pairs_upto(lines, usize::MAX)
}

/// [`split_pairs`], stopping at `max_rows`.
///
/// The fold tween's stand-in builds only the slice its clip can reveal and
/// re-renders every frame of the tween, so it must not pay to pair a 50k-line
/// hunk to draw twenty rows of it. Bounding the *output* is not enough — the
/// pending runs are bounded too, since a change block yields
/// `max(dels, adds)` rows and so anything past the budget can only land past
/// it as well.
pub fn split_pairs_upto(lines: &[DiffLine], max_rows: usize) -> Vec<LinePair> {
    /// The block being accumulated: the two sides' code lines, plus the
    /// `\ No newline…` marker each side may end on.
    #[derive(Default)]
    struct Block {
        dels: Vec<u32>,
        adds: Vec<u32>,
        del_meta: Vec<u32>,
        add_meta: Vec<u32>,
    }

    fn flush(pairs: &mut Vec<LinePair>, block: &mut Block, max_rows: usize) {
        let mut drain = |left: &mut Vec<u32>, right: &mut Vec<u32>| {
            for ix in 0..left.len().max(right.len()) {
                if pairs.len() >= max_rows {
                    break;
                }
                pairs.push((left.get(ix).copied(), right.get(ix).copied()));
            }
            left.clear();
            right.clear();
        };
        drain(&mut block.dels, &mut block.adds);
        // Markers trail the code they annotate, and pair with each other — a
        // modification where both files lost their final newline is one
        // aligned row plus one marker row, not two one-sided rows plus two
        // markers. They never share a row with code, so both render arms can
        // treat a marker on either side as spanning the row.
        drain(&mut block.del_meta, &mut block.add_meta);
    }

    let mut pairs = Vec::with_capacity(lines.len().min(max_rows));
    let mut block = Block::default();
    let mut pending_side: Option<LineKind> = None;
    for (ix, line) in lines.iter().enumerate() {
        match line.kind {
            LineKind::Del => {
                // A marker already closes its side, so code arriving after one
                // starts a fresh block — the marker row keeps its place in the
                // file's order.
                if !block.adds.is_empty()
                    || !block.del_meta.is_empty()
                    || !block.add_meta.is_empty()
                {
                    flush(&mut pairs, &mut block, max_rows);
                }
                let remaining = max_rows - pairs.len().min(max_rows);
                if remaining == 0 {
                    break;
                }
                if block.dels.len() < remaining {
                    block.dels.push(ix as u32);
                }
                pending_side = Some(LineKind::Del);
            }
            LineKind::Add => {
                // The old side's marker is the one case where a marker does
                // not close the block: `-old`, marker, `+new` is one edit.
                if !block.add_meta.is_empty() {
                    flush(&mut pairs, &mut block, max_rows);
                }
                let remaining = max_rows - pairs.len().min(max_rows);
                if remaining == 0 {
                    break;
                }
                if block.adds.len() < remaining {
                    block.adds.push(ix as u32);
                }
                pending_side = Some(LineKind::Add);
            }
            // `\ No newline at end of file` belongs to the side whose line it
            // follows, so it must NOT close the block: git writes `-old`,
            // marker, `+new`, marker for an edited last line, and treating
            // either marker as a boundary would tear that edit apart. A marker
            // after context describes the same line on both sides.
            LineKind::Meta => match pending_side {
                Some(LineKind::Del) => block.del_meta.push(ix as u32),
                Some(LineKind::Add) => block.add_meta.push(ix as u32),
                _ => {
                    block.del_meta.push(ix as u32);
                    block.add_meta.push(ix as u32);
                }
            },
            // Context sits on both sides.
            LineKind::Context => {
                flush(&mut pairs, &mut block, max_rows);
                if pairs.len() >= max_rows {
                    break;
                }
                pairs.push((Some(ix as u32), Some(ix as u32)));
                pending_side = Some(LineKind::Context);
            }
        }
    }
    flush(&mut pairs, &mut block, max_rows);
    pairs.truncate(max_rows);
    pairs
}

/// Every anchor a split row can *display* a card for, left column first.
///
/// Wider than what the row lets you write: only the right column takes a `+`
/// (see the `SplitLine` render arm), but an old-side note staged from the
/// unified layout must still show its card here, or toggling layouts would
/// look like it dropped one. A context row names the same anchor on both
/// sides, so the duplicate is dropped — the caller flattens, and two
/// identical anchors would stage the card twice. A fixed array, not a `Vec`:
/// this runs per row of every re-flatten.
fn pair_anchors(lines: &[DiffLine], pair: LinePair) -> [Option<(CommentSide, u32)>; 2] {
    let anchor = |ix: Option<u32>| {
        ix.and_then(|ix| lines.get(ix as usize))
            .and_then(line_anchor)
    };
    let (left, right) = (anchor(pair.0), anchor(pair.1));
    if left == right {
        [left, None]
    } else {
        [left, right]
    }
}

/// A deletion only exists in the pre-change file; everything else is cited
/// against the post-change file, which is what the agent edits.
pub fn line_anchor(line: &DiffLine) -> Option<(CommentSide, u32)> {
    match line.kind {
        LineKind::Meta => None,
        LineKind::Del => line.old_no.map(|no| (CommentSide::Old, no)),
        _ => line.new_no.map(|no| (CommentSide::New, no)),
    }
}

// ---------------------------------------------------------------------------
// Resolution + states (pure)
// ---------------------------------------------------------------------------

/// The diff shown for a chat: `checkout_id` match first, then device+cwd,
/// then cwd alone (§1.11).
pub fn resolve_diff<'a>(diffs: &'a [CheckoutDiff], chat: &Chat) -> Option<&'a CheckoutDiff> {
    if let Some(checkout_id) = chat.checkout_id.as_deref()
        && let Some(diff) = diffs.iter().find(|d| d.checkout_id == checkout_id)
    {
        return Some(diff);
    }
    let cwd = chat.cwd.as_deref()?;
    diffs
        .iter()
        .find(|d| d.device_id == chat.device_id && d.cwd == cwd)
        .or_else(|| diffs.iter().find(|d| d.cwd == cwd))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffPhase {
    /// No diff for this checkout yet.
    Preparing,
    /// Diff arrived and it's empty — working tree clean.
    Clean,
    List,
}

pub fn diff_phase(resolved: Option<&CheckoutDiff>) -> DiffPhase {
    match resolved {
        None => DiffPhase::Preparing,
        Some(diff) if diff.patch.trim().is_empty() && diff.files.is_empty() => DiffPhase::Clean,
        Some(_) => DiffPhase::List,
    }
}

// slopty: sentence case, as every label in the pane.
/// Header label: "N uncommitted change(s)".
pub fn uncommitted_label(count: usize) -> String {
    if count == 1 {
        "1 uncommitted change".to_string()
    } else {
        format!("{count} uncommitted changes")
    }
}

/// Which git colour a file header's count wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountKind {
    Added,
    Deleted,
    None,
}

// slopty: "+1 −0" read like raw data, the zero in red.
/// A file header's counts: `+N` when lines were added, `−N` when lines were removed, and a
/// single quiet "0" for a text file that changed neither (a mode change); none for a binary.
fn file_counts(adds: u32, dels: u32, binary: bool) -> Vec<(String, CountKind)> {
    let mut counts = Vec::new();
    if adds > 0 {
        counts.push((format!("+{adds}"), CountKind::Added));
    }
    if dels > 0 {
        counts.push((format!("−{dels}"), CountKind::Deleted));
    }
    if counts.is_empty() && !binary {
        counts.push(("0".to_owned(), CountKind::None));
    }
    counts
}

// slopty: a file's size of change at a glance, five squares after its counts (AgentCraft's
// `+19 −4 ■■■■■`; design-inspiration item 5), in the add and delete hues, never a grey bar.
/// How many squares of the five a change fills, and with what: as many as lines changed, up to
/// five, shared between added and removed in proportion, each side that changed holding one at
/// least; the rest stay empty.
fn diffstat_squares(adds: u32, dels: u32) -> [CountKind; 5] {
    let total = u64::from(adds) + u64::from(dels);
    let filled = total.min(5);
    let mut added = (filled * u64::from(adds) + total / 2).checked_div(total).unwrap_or(0);
    if adds > 0 {
        added = added.max(1);
    }
    if dels > 0 {
        added = added.min(filled - 1);
    }
    let mut squares = [CountKind::None; 5];
    for (ix, square) in squares.iter_mut().enumerate().take(filled as usize) {
        *square = if (ix as u64) < added { CountKind::Added } else { CountKind::Deleted };
    }
    squares
}

/// The five squares of [`diffstat_squares`], 7 pt with corners of 1.5, 2 apart.
pub(crate) fn diffstat_blocks(adds: u32, dels: u32, theme: &Theme) -> gpui::Div {
    div().flex_none().flex().flex_row().items_center().gap(px(2.0)).children(
        diffstat_squares(adds, dels).map(|kind| {
            div().size(px(7.0)).rounded(px(1.5)).bg(match kind {
                CountKind::Added => add_color(theme),
                CountKind::Deleted => del_color(theme),
                CountKind::None => theme.ink(0.12),
            })
        }),
    )
}

/// What the pane diffs against (t3code's scope dropdown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffScope {
    /// Uncommitted changes vs HEAD — the live watch stream.
    #[default]
    WorkingTree,
    /// Everything this branch adds over `merge-base(base_ref, HEAD)`,
    /// working tree included.
    Branch,
    /// Changes since the current chat's last turn started.
    LatestTurn,
    // slopty: any turn's, picked beside the scope menu (the turn is the pane's `turn`).
    /// The changes one earlier turn made.
    Turn,
    // slopty: no History surface (ruled out)
}

impl DiffScope {
    /// Scopes available from a Diff tab. History is selected from the surface
    /// picker instead, so it can keep its own tab and toolbar state.
    pub const ALL: [DiffScope; 3] = [Self::WorkingTree, Self::Branch, Self::LatestTurn];

    pub fn label(self) -> &'static str {
        match self {
            Self::WorkingTree => "Working tree",
            Self::Branch => "Branch changes",
            Self::LatestTurn => "Latest turn",
            Self::Turn => "Turn", // slopty: any turn's.
        }
    }

    /// Wire value for `GetCheckoutDiff` `mode` (and parse-key discriminant).
    pub fn mode(self) -> &'static str {
        match self {
            Self::WorkingTree => "workingTree",
            Self::Branch => "branch",
            Self::LatestTurn | Self::Turn => "turn", // slopty: any turn's.
        }
    }
}

/// Header-strip label per scope.
pub fn scope_label(scope: DiffScope, count: usize, base: Option<&str>) -> String {
    let files = if count == 1 { "file" } else { "files" };
    match scope {
        DiffScope::WorkingTree => uncommitted_label(count),
        DiffScope::Branch => match base {
            Some(base) => format!("{count} changed {files} vs {base}"),
            None => format!("{count} changed {files}"),
        },
        DiffScope::LatestTurn => format!("{count} changed {files} this turn"),
        DiffScope::Turn => format!("{count} changed {files} in this turn"), // slopty: any turn's.
    }
}

/// The comparison ref the branch scope preselects. `branches` comes from
/// `ListBranches` with the repo's default branch first — but a repo with no
/// `origin/HEAD` falls back to the *checked-out* branch there, and comparing a
/// branch with itself is useless; prefer `main`/`master` in that case.
pub fn default_base_ref(branches: &[String], current: Option<&str>) -> Option<String> {
    let first = branches.first()?;
    if current != Some(first.as_str()) {
        return Some(first.clone());
    }
    for candidate in ["main", "master"] {
        if branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }
    branches
        .iter()
        .find(|b| current != Some(b.as_str()))
        .or(Some(first))
        .cloned()
}

// slopty: a clean diff says what fills it, as Claude desktop's empty panes do.
/// The line under [`clean_message`]: what would show here.
pub fn clean_line(scope: DiffScope) -> &'static str {
    match scope {
        DiffScope::WorkingTree => "Edits in this folder show here.",
        DiffScope::Branch => "Commits on this branch show here.",
        DiffScope::LatestTurn | DiffScope::Turn => "This turn didn't change any files.",
    }
}

// slopty: one empty-state voice in the pane (`live::chrome::empty_state`, zeron's anatomy).
/// A diff with nothing to show, centred in the pane: its headline and the line under it.
fn pane_note(
    id: &'static str,
    headline: SharedString,
    line: Option<SharedString>,
    theme: &Theme,
) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .child(crate::live::chrome::empty_state(id, None, headline, line, None, theme))
        .into_any_element()
}

/// Empty-state copy per scope.
pub fn clean_message(scope: DiffScope, base: Option<&str>) -> String {
    match scope {
        DiffScope::WorkingTree => "No uncommitted changes".to_string(),
        DiffScope::Branch => match base {
            Some(base) => format!("No changes vs {base}"),
            None => "No branch changes".to_string(),
        },
        DiffScope::LatestTurn => "No changes this turn".to_string(),
        DiffScope::Turn => "No changes in this turn".to_string(), // slopty: any turn's.
    }
}

/// Fold a `WatchCheckoutDiffs` frame into the diff set. Accepts either a full
/// list (replace) or a single `CheckoutDiff` (upsert by checkout id) — the
/// contract streams `CheckoutDiff` items, but list frames cost nothing to
/// support. Returns whether anything changed.
pub fn apply_diff_frame(diffs: &mut Vec<CheckoutDiff>, value: serde_json::Value) -> bool {
    if let Ok(all) = serde_json::from_value::<Vec<CheckoutDiff>>(value.clone()) {
        if *diffs != all {
            *diffs = all;
            return true;
        }
        return false;
    }
    match serde_json::from_value::<CheckoutDiff>(value) {
        Ok(one) => {
            if let Some(existing) = diffs.iter_mut().find(|d| d.checkout_id == one.checkout_id) {
                if *existing == one {
                    return false;
                }
                *existing = one;
            } else {
                diffs.push(one);
            }
            true
        }
        Err(err) => {
            tracing::warn!(error = %err, "changes: dropping malformed diff frame");
            false
        }
    }
}

fn comment_state_key(
    comments: &[ReviewComment],
    draft: Option<&(String, CommentSide, u32)>,
) -> u64 {
    let mut parts: Vec<String> = comments
        .iter()
        .flat_map(|comment| [comment.id.clone(), comment.body.clone()])
        .collect();
    if let Some((path, side, line)) = draft {
        parts.push(format!("draft:{path}:{}:{line}", side.tag()));
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    hash64(&refs)
}

fn hash64(parts: &[&str]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for p in parts {
        p.hash(&mut hasher);
    }
    hasher.finish()
}

const MAX_EXCERPT_SOURCE_LINES: usize = 200_000;

fn excerpt_side(
    file: &FileDiff,
    side: SourceSide,
    language: Lang,
    path: &str,
) -> Option<Arc<slopty_syntax::HighlightedDocument>> {
    let max_line = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| match side {
            SourceSide::Old => line.old_no,
            SourceSide::New => line.new_no,
        })
        .max()
        .unwrap_or(0) as usize;
    if max_line > MAX_EXCERPT_SOURCE_LINES {
        return None;
    }
    let mut lines = vec![Vec::new(); max_line];
    for hunk in &file.hunks {
        let visible = hunk
            .lines
            .iter()
            .filter_map(|line| {
                let number = match side {
                    SourceSide::Old => line.old_no,
                    SourceSide::New => line.new_no,
                }?;
                (line.kind != LineKind::Meta).then_some((number, line.text.as_str()))
            })
            .collect::<Vec<_>>();
        if visible.is_empty() {
            continue;
        }
        let source = visible
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join("\n");
        let document = slopty_syntax::highlight(slopty_syntax::HighlightRequest {
            source: &source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()?;
        for ((number, _), spans) in visible.into_iter().zip(document.lines) {
            lines[number as usize - 1] = spans;
        }
    }
    Some(Arc::new(slopty_syntax::HighlightedDocument {
        language,
        lines,
    }))
}

pub(crate) fn excerpt_highlights(file: &FileDiff, language: Lang) -> Option<DiffHighlights> {
    if !slopty_syntax::supports_language(language) {
        return None;
    }
    let old = if file.status == FileStatus::Added {
        None
    } else {
        Some(excerpt_side(
            file,
            SourceSide::Old,
            language,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?)
    };
    let new = if file.status == FileStatus::Deleted {
        None
    } else {
        Some(excerpt_side(file, SourceSide::New, language, &file.path)?)
    };
    Some(DiffHighlights { old, new })
}

fn sources_match_patch(file: &FileDiff, response: &slopty_view_proto::CheckoutFileDiffText) -> bool {
    let old = response
        .old_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    let new = response
        .new_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    file.hunks.iter().flat_map(|hunk| &hunk.lines).all(|line| {
        let actual = match line.kind {
            LineKind::Del => line
                .old_no
                .and_then(|number| old.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Add => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Context => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied())
                .or_else(|| {
                    line.old_no
                        .and_then(|number| old.as_ref()?.get(number as usize - 1).copied())
                }),
            LineKind::Meta => return true,
        };
        actual == Some(line.text.as_str())
    })
}

/// slopty: the new side of `file` line by line, when `response` is whole and still the patch's.
fn new_side_lines(
    file: &FileDiff,
    response: &slopty_view_proto::CheckoutFileDiffText,
) -> Option<Arc<Vec<String>>> {
    if response.stale
        || response.binary
        || response.truncated
        || !sources_match_patch(file, response)
    {
        return None;
    }
    let new = response.new_text.as_deref()?;
    Some(Arc::new(new.lines().map(str::to_owned).collect()))
}

fn full_highlights(
    file: &FileDiff,
    language: Lang,
    response: &slopty_view_proto::CheckoutFileDiffText,
) -> Option<DiffHighlights> {
    if response.stale
        || response.binary
        || response.truncated
        || !sources_match_patch(file, response)
    {
        return None;
    }
    let parse = |source: &str, path: &str| {
        slopty_syntax::highlight(slopty_syntax::HighlightRequest {
            source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()
        .map(Arc::new)
    };
    let old = match response.old_text.as_deref() {
        Some(source) => Some(parse(
            source,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?),
        None => None,
    };
    let new = match response.new_text.as_deref() {
        Some(source) => Some(parse(source, &file.path)?),
        None => None,
    };
    if old.is_none() && new.is_none() && slopty_syntax::supports_language(language) {
        return None;
    }
    Some(DiffHighlights { old, new })
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

struct MeasuredDiffWidth {
    key: (u32, u32, u32, SharedString),
    // Retaining the Arc makes pointer identity safe against allocator reuse.
    highlights: Option<Arc<DiffHighlights>>,
    width: f32,
}

struct FileHorizontalState {
    geometry: DiffHorizontalGeometry,
    scroll: gpui::ScrollHandle,
    /// Shape once per file, typography/theme, and highlight revision.
    measured: std::cell::RefCell<Option<MeasuredDiffWidth>>,
}

impl FileHorizontalState {
    fn new(file: &FileDiff) -> Self {
        Self {
            geometry: DiffHorizontalGeometry::from_file(file),
            scroll: gpui::ScrollHandle::new(),
            measured: std::cell::RefCell::new(None),
        }
    }

    fn metrics(
        &self,
        file: &FileDiff,
        highlights: Option<&Arc<DiffHighlights>>,
        theme: &Theme,
        text_system: &gpui::WindowTextSystem,
        generation: u32,
    ) -> DiffHorizontalMetrics {
        let key = (
            generation,
            crate::theme::style_generation(),
            diff_text_size(theme).to_bits(),
            theme.font_mono.clone(),
        );
        let mut cached = self.measured.borrow_mut();
        let current = cached.as_ref().is_some_and(|cached| {
            cached.key == key
                && match (cached.highlights.as_ref(), highlights) {
                    (None, None) => true,
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                }
        });
        // slopty: the width measured now or kept, with no unwrap.
        let width = match cached.as_ref() {
            Some(kept) if current => kept.width,
            _ => {
                let width = max_shaped_text_width(
                    file,
                    highlights.map(AsRef::as_ref),
                    theme,
                    text_system,
                );
                *cached = Some(MeasuredDiffWidth { key, highlights: highlights.cloned(), width });
                width
            }
        };
        DiffHorizontalMetrics {
            max_text_width: width,
            max_gutter_width: self.geometry.max_gutter_width,
        }
    }
}

struct ParsedDiff {
    /// `checkout_id:checksum` — identity of the parsed content.
    key: String,
    truncated: bool,
    additions: u32,
    deletions: u32,
    file_count: usize,
    /// Indexed like `files`; survives row virtualization and folding.
    horizontal: Vec<FileHorizontalState>,
    files: Arc<Vec<FileDiff>>,
}

// ---------------------------------------------------------------------------
// Row model — the diff flattened to line granularity (pure)
// ---------------------------------------------------------------------------

/// One virtualized list row. The diff is flattened so each visible LINE is
/// its own row (Zed's editor draws exactly the visible line range the same
/// way): scrolling a 10k-line file materializes ~50 line rows per frame, not
/// one 10k-line element, and a collapsed file contributes no body rows at
/// all. Nowrap heights use the analytic constants above; wrapped line rows
/// are measured by the list at the current pane width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    FileHeader {
        file: u32,
    },
    Notice {
        file: u32,
        notice: u32,
    },
    /// slopty: the unchanged lines above hunk `hunk`, as a count (item 11).
    Unchanged {
        file: u32,
        hunk: u32,
    },
    Line {
        file: u32,
        hunk: u32,
        line: u32,
        /// Flat index across the file's hunks — keys into the highlight slot.
        flat: u32,
    },
    /// One split row: the two line indices [`split_pairs`] paired. Carrying
    /// them inline keeps the pairing off the render path — it is computed
    /// once, when the body is flattened.
    SplitLine {
        file: u32,
        hunk: u32,
        left: Option<u32>,
        right: Option<u32>,
    },
    /// `card` indexes the file's own staged-comment slice, in staged order.
    CommentCard {
        file: u32,
        card: u32,
    },
    CommentDraft {
        file: u32,
    },
    /// Trailing pad closing an expanded body ([`BODY_BOTTOM_PAD`]).
    BodyPad {
        file: u32,
    },
    /// A body mid-fold-tween: one height-animated, clipped row standing in
    /// for the whole body. Only the slice that can be revealed is built —
    /// the tween never pays for off-screen lines.
    FoldingBody {
        file: u32,
    },
}

impl DiffRow {
    fn file(self) -> usize {
        match self {
            Self::FileHeader { file }
            | Self::Notice { file, .. }
            | Self::Unchanged { file, .. }
            | Self::Line { file, .. }
            | Self::SplitLine { file, .. }
            | Self::CommentCard { file, .. }
            | Self::CommentDraft { file }
            | Self::BodyPad { file }
            | Self::FoldingBody { file } => file as usize,
        }
    }

    /// `FoldingBody` is height-animated, so it reports 0 and never lands in a
    /// height sum.
    fn height(self, comments: &[ReviewComment], line_h: f32) -> f32 {
        match self {
            DiffRow::FileHeader { .. } => FILE_HEADER_HEIGHT,
            DiffRow::Notice { .. } => NOTICE_HEIGHT,
            DiffRow::Unchanged { .. } => UNCHANGED_HEIGHT,
            DiffRow::Line { .. } | DiffRow::SplitLine { .. } => line_h,
            DiffRow::CommentCard { card, .. } => comments
                .get(card as usize)
                .map(|comment| comments::card_height(&comment.body))
                .unwrap_or(0.0),
            DiffRow::CommentDraft { .. } => comments::DRAFT_CARD_HEIGHT,
            DiffRow::BodyPad { .. } => BODY_BOTTOM_PAD,
            DiffRow::FoldingBody { .. } => 0.0,
        }
    }
}

/// Capacity hint only — comment cards are not counted. Split pairs can only
/// shrink the line count, so the unified count is a safe hint for both.
pub fn body_row_count(file: &FileDiff) -> usize {
    let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    // slopty: only a hunk with unchanged lines above it gets their row (`body_rows`).
    let gaps = (0..file.hunks.len()).filter(|ix| unchanged_before(file, *ix) > 0).count();
    file_notices(file).len() + gaps + lines + 1
}

pub fn body_rows(
    file_ix: u32,
    file: &FileDiff,
    comments: &[ReviewComment],
    draft: Option<(CommentSide, u32)>,
    mode: DiffMode,
) -> Vec<DiffRow> {
    fn push_cards(
        rows: &mut Vec<DiffRow>,
        file_ix: u32,
        comments: &[ReviewComment],
        draft: Option<(CommentSide, u32)>,
        anchors: &[Option<(CommentSide, u32)>],
    ) {
        for anchor in anchors.iter().flatten() {
            for (ix, comment) in comments.iter().enumerate() {
                if comment.diff_anchor() == Some(*anchor) {
                    rows.push(DiffRow::CommentCard {
                        file: file_ix,
                        card: ix as u32,
                    });
                }
            }
            if draft == Some(*anchor) {
                rows.push(DiffRow::CommentDraft { file: file_ix });
            }
        }
    }

    let mut rows = Vec::with_capacity(body_row_count(file));
    for notice in 0..file_notices(file).len() {
        rows.push(DiffRow::Notice {
            file: file_ix,
            notice: notice as u32,
        });
    }
    let mut hunk_flat = 0u32;
    for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
        if unchanged_before(file, hunk_ix) > 0 {
            rows.push(DiffRow::Unchanged {
                file: file_ix,
                hunk: hunk_ix as u32,
            });
        }
        match mode {
            DiffMode::Unified => {
                for (line_ix, line) in hunk.lines.iter().enumerate() {
                    rows.push(DiffRow::Line {
                        file: file_ix,
                        hunk: hunk_ix as u32,
                        line: line_ix as u32,
                        flat: hunk_flat + line_ix as u32,
                    });
                    push_cards(&mut rows, file_ix, comments, draft, &[line_anchor(line)]);
                }
            }
            DiffMode::Split => {
                for (left, right) in split_pairs(&hunk.lines) {
                    rows.push(DiffRow::SplitLine {
                        file: file_ix,
                        hunk: hunk_ix as u32,
                        left,
                        right,
                    });
                    let anchors = pair_anchors(&hunk.lines, (left, right));
                    push_cards(&mut rows, file_ix, comments, draft, &anchors);
                }
            }
        }
        hunk_flat += hunk.lines.len() as u32;
    }
    rows.push(DiffRow::BodyPad { file: file_ix });
    rows
}

/// Flatten all files into rows + each file's row span (header at
/// `range.start`, body rows after it). `collapsed(ix)` folds a file to just
/// its header. `comments` is the whole staged set; each file takes its own
/// path's slice.
pub fn flatten_rows(
    files: &[FileDiff],
    comments: &[ReviewComment],
    draft: Option<(&str, CommentSide, u32)>,
    mode: DiffMode,
    mut collapsed: impl FnMut(usize) -> bool,
) -> (Vec<DiffRow>, Vec<std::ops::Range<usize>>) {
    let mut rows = Vec::new();
    let mut ranges = Vec::with_capacity(files.len());
    for (ix, file) in files.iter().enumerate() {
        let start = rows.len();
        rows.push(DiffRow::FileHeader { file: ix as u32 });
        if !collapsed(ix) {
            let file_comments: Vec<ReviewComment> = comments
                .iter()
                .filter(|comment| !comment.is_file() && comment.path == file.path)
                .cloned()
                .collect();
            let file_draft = draft
                .filter(|(path, _, _)| *path == file.path)
                .map(|(_, side, line)| (side, line));
            rows.extend(body_rows(ix as u32, file, &file_comments, file_draft, mode));
        }
        ranges.push(start..rows.len());
    }
    (rows, ranges)
}

/// The file header that should remain visible for a logical list position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StickyFileHeader {
    file_ix: usize,
    header_row: usize,
    next_header_row: Option<usize>,
}

/// Resolve a sticky file header from the current flattened row ranges.
///
/// This remains independent of the rendered list so folds and diff resets
/// cannot leave a second, stale active-file state behind.
fn sticky_file_header(
    row_ranges: &[std::ops::Range<usize>],
    item_ix: usize,
    offset_in_item: f32,
) -> Option<StickyFileHeader> {
    let file_ix = row_ranges
        .partition_point(|range| range.start <= item_ix)
        .checked_sub(1)?;
    let range = row_ranges.get(file_ix)?;

    // A reset can briefly leave ListState pointing past the replacement
    // model. Treat that frame as having no sticky header.
    if !range.contains(&item_ix) || (item_ix == range.start && offset_in_item <= 0.0) {
        return None;
    }

    Some(StickyFileHeader {
        file_ix,
        header_row: range.start,
        next_header_row: row_ranges.get(file_ix + 1).map(|range| range.start),
    })
}

/// Offset a sticky header upward as the next file header enters its slot.
pub(crate) fn sticky_header_push_offset(next_header_y: Option<f32>) -> f32 {
    next_header_y
        .map(|y| (y - FILE_HEADER_HEIGHT).min(0.0))
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileHeaderPresentation {
    Row,
    Sticky,
}

impl FileHeaderPresentation {
    fn key_prefix(self) -> &'static str {
        match self {
            Self::Row => "file-hdr",
            Self::Sticky => "sticky-file-hdr",
        }
    }

    fn element_id(self, file_ix: usize) -> SharedString {
        let prefix = self.key_prefix();
        SharedString::from(format!("{prefix}-{file_ix}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StickyFileHeaderPaint {
    pub(crate) rest_bg: gpui::Hsla,
    pub(crate) hover_bg: gpui::Hsla,
    pub(crate) border: gpui::Hsla,
    pub(crate) frost_tint: Option<gpui::Hsla>,
}

/// Resolve the sticky header from the diff's content plane, not the elevated
/// overlay plane used by menus and popovers.
pub(crate) fn sticky_file_header_paint(theme: &Theme) -> StickyFileHeaderPaint {
    if theme.is_frost() {
        let tint_alpha = match theme.appearance {
            crate::theme::Appearance::Dark => STICKY_FILE_HEADER_TINT_ALPHA_DARK,
            crate::theme::Appearance::Light => STICKY_FILE_HEADER_TINT_ALPHA_LIGHT,
        };
        StickyFileHeaderPaint {
            rest_bg: theme.ink(0.025),
            hover_bg: theme.glass_hover(),
            border: theme.border,
            frost_tint: Some(theme.bg.opacity(tint_alpha)),
        }
    } else {
        StickyFileHeaderPaint {
            rest_bg: crate::theme::flatten(theme.ink(0.025), theme.bg),
            hover_bg: crate::theme::flatten(theme.element_hover, theme.bg),
            border: theme.border,
            frost_tint: None,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct FileFold {
    collapsed: bool,
    // slopty: collapsed because the diff is large ([`held_back`]), until loaded.
    held: bool,
    /// Bumped per toggle — keys the height tween + chevron transition.
    epoch: usize,
    from: f32,
    to: f32,
    /// When the toggle happened: the tweens are armed only briefly after the
    /// click — gpui replays an element's animation on remount, and in the
    /// virtualized list a row scrolling back into view is a remount (the
    /// transcript's tool groups had the same flash; user report).
    toggled_at: Option<std::time::Instant>,
}

/// Tween arming window after a fold toggle (the fold's 300 ms plus margin).
const FOLD_TWEEN_WINDOW: Duration = Duration::from_millis(400);

/// Ceiling on how much body a fold tween's stand-in row materializes. A
/// tween always starts from a clicked (on-screen) header, so the revealable
/// slice is at most one viewport tall — everything past this is clipped or
/// below the fold either way.
const FOLD_TWEEN_MAX_PX: f32 = 2400.0;

impl FileFold {
    fn animating(&self, now: std::time::Instant) -> bool {
        self.epoch > 0
            && self
                .toggled_at
                .is_some_and(|at| now.saturating_duration_since(at) < FOLD_TWEEN_WINDOW)
    }
}

struct HighlightSlot {
    fingerprint: u64,
    state: DiffHighlightState,
    _excerpt_task: Option<Task<()>>,
    _fetch_task: Option<Task<()>>,
}

enum DiffHighlightState {
    Pending,
    Ready(Arc<DiffHighlights>),
    Excerpt(Arc<DiffHighlights>),
    Plain,
}

/// The open base-ref dropdown — the same searchable-menu recipe as the
/// composer's ref picker and the spaces filter: a filter input on top
/// (`PaletteSearch` context so ↑↓/⏎ bubble to the card's key handler),
/// ranked substring rows below.
struct RefMenu {
    search: Entity<ComposerInput>,
    /// Keyboard highlight within the filtered rows.
    active: usize,
    /// Tracked on the card — puts it on the keyboard dispatch path while the
    /// search input holds focus (the structure every working picker uses).
    focus: FocusHandle,
    _search_events: Subscription,
}

/// The line the pointer is on. Only one element per anchor ever takes the
/// hover — the unified row, or a split row's right column — so the anchor
/// alone identifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HoverRow {
    path: String,
    side: CommentSide,
    line: u32,
}

struct CommentDraft {
    editing_id: Option<String>,
    /// Composer the note will stage onto, captured when the card opened. A
    /// draft belongs to the checkout it was written over, so it must not
    /// follow the user onto whatever chat is selected by commit time.
    key: String,
    path: String,
    /// The file's pre-rename path, when it moved — carried onto the comment so
    /// an `Old`-side citation names the file that line lives in.
    old_path: Option<String>,
    side: CommentSide,
    line: u32,
    input: Entity<ComposerInput>,
    _events: Subscription,
}

// slopty: the find bar over the diff: its field, and the card holding the keyboard while the
// field has it (↑↓ and ⏎ bubble to it).
struct DiffFind {
    input: Entity<ComposerInput>,
    focus: FocusHandle,
    _events: Subscription,
}

/// The Changes pane entity. Lazy: no RPC until [`Changes::ensure_watch`] runs
/// (the shell calls it when the pane first opens).
pub struct Changes {
    state: Entity<AppState>,
    diffs: Vec<CheckoutDiff>,
    started: bool,
    error: Option<SharedString>,
    /// Device the running watch targets: `None` = the connected engine itself,
    /// `Some(id)` = a remote chat's host (relay-forwarded). The stream only
    /// carries the TARGET device's checkouts, so a selection change onto a
    /// chat hosted elsewhere tears the watch down and re-subscribes.
    watch_target: Option<String>,
    watch_task: Option<Task<()>>,
    parsed: Option<ParsedDiff>,
    parse_task: Option<Task<()>>,
    folds: HashMap<String, FileFold>,
    highlights: HashMap<String, HighlightSlot>,
    /// slopty: each file's new side, line by line, once a click on an unchanged-lines row
    /// fetched it, and the fetch in flight (item 11).
    sources: HashMap<String, Arc<Vec<String>>>,
    source_task: Option<Task<()>>,
    /// slopty: each hunk's changed words, by file and hunk, worked out as its lines first draw
    /// (`words::hunk_words`; design-inspiration item 14).
    words: HashMap<(u32, u32), Arc<Vec<Vec<std::ops::Range<usize>>>>>,
    /// The flattened row model the list virtualizes over (line granularity;
    /// collapsed bodies excluded) + each file's row span within it.
    rows: Vec<DiffRow>,
    row_ranges: Vec<std::ops::Range<usize>>,
    /// Sweeps [`DiffRow::FoldingBody`] stand-ins back to steady-state rows
    /// once their tween window elapses.
    fold_settle: Option<Task<()>>,
    list: ListState,
    /// What the pane diffs against (toolbar dropdown).
    scope: DiffScope,
    /// Unified or side-by-side (toolbar toggle, persisted per user).
    mode: DiffMode,
    /// Wrap long source lines instead of exposing the horizontal code plane.
    wrap_lines: bool,
    /// Comparison ref for [`DiffScope::Branch`] — preset to the repo's
    /// default branch once the branch list lands.
    base_ref: Option<String>,
    branches: Vec<String>,
    /// `device:cwd` the branch list was fetched for.
    branches_for: Option<String>,
    branches_task: Option<Task<()>>,
    /// One-shot scoped capture (Branch / Latest turn) + its fetch key.
    scoped: Option<CheckoutDiff>,
    scoped_for: Option<String>,
    scoped_error: Option<SharedString>,
    scoped_inflight: Option<String>,
    scoped_task: Option<Task<()>>,
    scope_menu: Popup<()>,
    ref_menu: Popup<RefMenu>,
    /// The ref menu's list scroll and rail. Lives outside [`RefMenu`] so the
    /// rail can reach it mutably while the card plays its exit.
    ref_menu_scroll: crate::settings::widgets::PageScroll,
    /// Only ever one: a second `+` moves the card rather than stacking two
    /// half-written notes.
    draft: Option<CommentDraft>,
    hover: Option<HoverRow>,
    comment_key: u64,
    // slopty: any turn's diff, find in the diff, whitespace and the changed files' tree.
    /// The turn [`DiffScope::Turn`] shows, by the id of the person's message that began it.
    turn: Option<String>,
    turn_menu: Popup<()>,
    ignore_whitespace: bool,
    show_tree: bool,
    find: Option<DiffFind>,
    found: Vec<DiffMatch>,
    found_active: Option<usize>,
    /// The parse key and query `found` was made for.
    found_for: Option<(String, String)>,
    /// The pane's own key context (`Changes`), where the find's actions land.
    focus: FocusHandle,
    /// The keyboard goes back to the pane on the next frame (the find was put away).
    refocus: bool,
    tree_scroll: gpui::ScrollHandle,
    // slopty: the turn list's scroll, and the one rail the review aids share: on the turn menu
    // while it is open, else on the tree (M26).
    turn_list_scroll: gpui::ScrollHandle,
    aid_rail: popover::MenuScrollbarState,
    /// The file last picked in the tree.
    tree_selected: Option<usize>,
    _requested: Subscription,
    // slopty: no History surface (ruled out)
    _observe: Subscription,
}

// slopty: no Discard changes: the person reviews, agents fix (ruled out).

/// Events the host (the right pane's surface strip) listens for.
pub enum ChangesEvent {
    // slopty: no History surface (ruled out)
    /// Open the post-change path in the workspace file browser.
    OpenFile(String),
    // slopty: no Discard changes (ruled out).
}

impl gpui::EventEmitter<ChangesEvent> for Changes {}

impl Changes {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
        let settings = crate::settings::current(cx);
        let mode = DiffMode::from_split(settings.diff_split);
        // slopty: a turn a transcript asks for while the tab is up, and the review aids' last
        // choices.
        let requested =
            cx.observe_global::<RequestedTurn>(|this: &mut Self, cx| this.take_requested_turn(cx));
        let mut this = Self {
            state,
            mode,
            wrap_lines: settings.diff_wrap,
            diffs: Vec::new(),
            started: false,
            error: None,
            watch_target: None,
            watch_task: None,
            parsed: None,
            parse_task: None,
            folds: HashMap::new(),
            highlights: HashMap::new(),
            words: HashMap::new(),
            sources: HashMap::new(),
            source_task: None,
            rows: Vec::new(),
            row_ranges: Vec::new(),
            fold_settle: None,
            // Rows are single lines now — a deep overdraw is cheap and keeps
            // fast wheel flicks from outrunning measurement.
            list: ListState::new(0, ListAlignment::Top, px(1024.0)),
            scope: DiffScope::default(),
            base_ref: None,
            branches: Vec::new(),
            branches_for: None,
            branches_task: None,
            scoped: None,
            scoped_for: None,
            scoped_error: None,
            scoped_inflight: None,
            scoped_task: None,
            scope_menu: Popup::default(),
            ref_menu: Popup::default(),
            ref_menu_scroll: Default::default(),
            draft: None,
            hover: None,
            comment_key: 0,
            turn: None,
            turn_menu: Popup::default(),
            // slopty: the review aids as the person last chose them, kept across launches.
            ignore_whitespace: settings.diff_ignore_whitespace,
            show_tree: settings.diff_show_tree,
            find: None,
            found: Vec::new(),
            found_active: None,
            found_for: None,
            focus: cx.focus_handle(),
            refocus: false,
            tree_scroll: gpui::ScrollHandle::new(),
            turn_list_scroll: gpui::ScrollHandle::new(),
            aid_rail: popover::MenuScrollbarState::default(),
            tree_selected: None,
            _requested: requested,
            _observe: observe,
        };
        this.take_requested_turn(cx); // slopty: a tab opened for a turn starts on it.
        this
    }

    // slopty: no History surface (ruled out)

    /// The surface-tab title: the scope's label. // slopty: no History surface (ruled out)
    pub fn tab_title(&self) -> gpui::SharedString {
        gpui::SharedString::from(self.scope.label())
    }

    /// The selected chat's host device when it differs from the connected
    /// engine's own — diffs are produced where the checkout lives, so a
    /// remote chat's watch must relay-forward (`targetDeviceId`) to its host.
    /// Without this the local stream simply never carries the remote checkout
    /// and the pane sits on "Preparing diff…" forever (user report).
    fn desired_target(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let device = state.selected_chat_row()?.device_id.clone();
        (state.local_device_id.as_deref() != Some(device.as_str())).then_some(device)
    }

    /// Start the `WatchCheckoutDiffs` subscription (idempotent per target).
    /// Retries with a flat 2 s delay if the stream fails or ends; the last
    /// content stays visible under an error banner meanwhile.
    pub fn ensure_watch(&mut self, cx: &mut Context<Self>) {
        let target = self.desired_target(cx);
        if self.started && self.watch_target == target {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            // Engine still booting — retry on the next state change via sync().
            return;
        };
        // Retarget: the old task (and its stream) drop; rows from the previous
        // device would resolve against the wrong checkouts, so clear them.
        if self.started {
            self.diffs.clear();
            self.error = None;
        }
        self.started = true;
        self.watch_target = target.clone();
        self.watch_task = Some(Self::spawn_watch(engine, target, cx));
    }

    fn spawn_watch(
        engine: EngineHandle,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let mut params = serde_json::Map::new();
                if let Some(target) = &target {
                    params.insert(
                        "targetDeviceId".into(),
                        serde_json::Value::String(target.clone()),
                    );
                }
                let subscribed = engine
                    .client()
                    .subscribe(
                        methods::WATCH_CHECKOUT_DIFFS,
                        serde_json::Value::Object(params),
                    )
                    .await;
                match subscribed {
                    Ok(mut rx) => {
                        while let Some(value) = rx.recv().await {
                            let alive = this.update(cx, |changes, cx| {
                                changes.error = None;
                                if apply_diff_frame(&mut changes.diffs, value) {
                                    changes.sync(cx);
                                    cx.notify();
                                }
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        // Stream ended (engine restart / reconnect): banner + retry.
                        if this
                            .update(cx, |changes, cx| {
                                changes.error = Some("Diff stream interrupted — retrying".into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(err) => {
                        if this
                            .update(cx, |changes, cx| {
                                changes.error =
                                    Some(format!("Diff watch unavailable: {err}").into());
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        })
    }

    fn resolved(&self, cx: &App) -> Option<CheckoutDiff> {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        resolve_diff(&self.diffs, chat).cloned()
    }

    /// The checkout root the scoped RPCs address: the watch-resolved diff's
    /// canonical cwd when available, else the chat row's own.
    fn scoped_cwd(&self, cx: &App) -> Option<String> {
        if let Some(diff) = self.resolved(cx) {
            return Some(diff.cwd);
        }
        self.state.read(cx).selected_chat_row()?.cwd.clone()
    }

    /// The diff the pane currently displays: the watch stream for the working
    /// tree, the one-shot scoped capture otherwise.
    fn active_diff(&self, cx: &App) -> Option<CheckoutDiff> {
        match self.scope {
            DiffScope::WorkingTree => self.resolved(cx),
            DiffScope::Branch | DiffScope::LatestTurn | DiffScope::Turn => self.scoped.clone(), // slopty: no History surface (ruled out)
        }
    }

    // slopty: no Discard changes (ruled out).

    /// Scope discriminant folded into the parse key, so a scope or base
    /// switch re-parses even when checksums collide.
    fn scope_key(&self) -> String {
        match self.scope {
            DiffScope::WorkingTree => "wt".to_string(),
            DiffScope::Branch => format!("br:{}", self.base_ref.as_deref().unwrap_or("")),
            DiffScope::LatestTurn => "turn".to_string(),
            DiffScope::Turn => format!("turn:{}", self.turn.as_deref().unwrap_or("")), // slopty: any turn's.
        }
    }

    fn parse_key(&self, diff: &CheckoutDiff) -> String {
        format!(
            "{}:{}:{}{}",
            diff.checkout_id,
            diff.checksum,
            self.scope_key(),
            if self.ignore_whitespace { ":w" } else { "" } // slopty: whitespace ignored.
        )
    }

    /// Fetch the branch list for the selected chat's checkout (idempotent per
    /// device+cwd); the repo's default branch (first entry) becomes the
    /// comparison base unless the user already picked one that still exists.
    fn ensure_branches(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let target = self.desired_target(cx);
        let key = format!("{}:{}", target.as_deref().unwrap_or("local"), cwd);
        if self.branches_for.as_deref() == Some(key.as_str()) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.branches_for = Some(key.clone());
        self.branches_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("repoPath".into(), serde_json::Value::String(cwd));
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(methods::LIST_BRANCHES, serde_json::Value::Object(params))
                .await;
            this.update(cx, |changes, cx| {
                if changes.branches_for.as_deref() != Some(key.as_str()) {
                    return; // superseded by a chat/device switch
                }
                match result {
                    Ok(value) => {
                        changes.branches =
                            serde_json::from_value::<Vec<String>>(value).unwrap_or_default();
                        let keep = changes
                            .base_ref
                            .as_ref()
                            .is_some_and(|base| changes.branches.contains(base));
                        if !keep {
                            let current = changes
                                .state
                                .read(cx)
                                .selected_chat_row()
                                .and_then(|chat| chat.branch.clone());
                            changes.base_ref =
                                default_base_ref(&changes.branches, current.as_deref());
                        }
                        changes.sync(cx);
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "changes: branch list failed");
                        // Allow a retry on the next state change.
                        changes.branches_for = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Keep the one-shot scoped capture fresh. The fetch key folds in the
    /// watch checksum, so any working-tree change (or commit — HEAD rides the
    /// checksum) re-captures; a context change (chat/scope/base) clears the
    /// stale content first so the pane shows the spinner, while a
    /// checksum-only refresh keeps the old diff visible until the new one
    /// lands.
    fn ensure_scoped(&mut self, cx: &mut Context<Self>) {
        if matches!(self.scope, DiffScope::WorkingTree) { // slopty: no History surface (ruled out)
            self.scoped_inflight = None;
            self.scoped_task = None;
            return;
        }
        let Some(chat_id) = self
            .state
            .read(cx)
            .selected_chat_row()
            .map(|chat| chat.id.clone())
        else {
            return;
        };
        let Some(cwd) = self.scoped_cwd(cx) else {
            return;
        };
        let base = match self.scope {
            DiffScope::Branch => match &self.base_ref {
                Some(base) => Some(base.clone()),
                None => return, // branch list still loading
            },
            _ => None,
        };
        let target = self.desired_target(cx);
        // slopty: the picked turn, and whether whitespace counts.
        let turn = (self.scope == DiffScope::Turn).then(|| self.turn.clone()).flatten();
        let ignore_whitespace = self.ignore_whitespace;
        let context = format!(
            "{}|{}|{}|{}|{}|{}|{}",
            target.as_deref().unwrap_or("local"),
            chat_id,
            cwd,
            self.scope.mode(),
            base.as_deref().unwrap_or(""), // slopty: no commit scope
            turn.as_deref().unwrap_or(""),
            ignore_whitespace
        );
        let watch_sum = self.resolved(cx).map(|d| d.checksum).unwrap_or_default();
        let key = format!("{context}|{watch_sum}");
        if self.scoped_for.as_deref() == Some(key.as_str())
            || self.scoped_inflight.as_deref() == Some(key.as_str())
        {
            return;
        }
        if self
            .scoped_for
            .as_deref()
            .is_none_or(|prev| !prev.starts_with(&format!("{context}|")))
        {
            self.scoped = None;
            self.scoped_error = None;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mode = self.scope.mode();
        self.scoped_inflight = Some(key.clone());
        self.scoped_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            params.insert("cwd".into(), serde_json::Value::String(cwd));
            params.insert("mode".into(), serde_json::Value::String(mode.to_string()));
            params.insert("chatId".into(), serde_json::Value::String(chat_id));
            if let Some(base) = base {
                params.insert("baseRef".into(), serde_json::Value::String(base));
            }
            // slopty: which turn, and whitespace ignored (`git diff -w`).
            scoped_params(&mut params, turn, ignore_whitespace);
            if let Some(target) = target {
                params.insert("targetDeviceId".into(), serde_json::Value::String(target));
            }
            let result = engine
                .client()
                .call(
                    methods::GET_CHECKOUT_DIFF,
                    serde_json::Value::Object(params),
                )
                .await;
            this.update(cx, |changes, cx| {
                if changes.scoped_inflight.as_deref() != Some(key.as_str()) {
                    return; // superseded
                }
                changes.scoped_inflight = None;
                match result.and_then(|value| {
                    serde_json::from_value::<CheckoutDiff>(value)
                        .map_err(|e| slopty_view_rpc::RpcError::Failed(e.to_string()))
                }) {
                    Ok(diff) => {
                        changes.scoped = Some(diff);
                        changes.scoped_error = None;
                    }
                    Err(err) => {
                        changes.scoped = None;
                        changes.scoped_error = Some(err.to_string().into());
                    }
                }
                changes.scoped_for = Some(key);
                changes.sync(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn reset_horizontal_scroll(&self) {
        if let Some(parsed) = &self.parsed {
            for file in &parsed.horizontal {
                reset_horizontal_scroll(&file.scroll);
            }
        }
    }

    fn set_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.reset_horizontal_scroll(); // slopty: no History surface (ruled out)
            self.sync(cx);
        }
        cx.notify();
    }

    // slopty: no History surface (ruled out)

    fn set_base_ref(&mut self, base: String, cx: &mut Context<Self>) {
        if self.base_ref.as_deref() != Some(base.as_str()) {
            self.base_ref = Some(base);
            self.sync(cx);
        }
        cx.notify();
    }

    /// slopty: the person brought the pane to the front: the keyboard goes to it on its next
    /// frame (once a menu that opened it has let go), so ⌘F finds in the diff.
    pub(crate) fn take_keyboard(&mut self, cx: &mut Context<Self>) {
        self.refocus = true;
        cx.notify();
    }

    /// Everything the pane needs kicked when (re)shown: the watch plus the
    /// scope-specific loads (branches, scoped/commit capture, history) — the
    /// shell's hook for freshly-mounted surface tabs.
    pub fn ensure_content(&mut self, cx: &mut Context<Self>) {
        self.sync(cx);
    }

    /// slopty: tests: the working tree of the chat in `cwd` reads `patch`.
    #[cfg(test)]
    pub(crate) fn show_patch(&mut self, cwd: &str, patch: &str, cx: &mut Context<Self>) {
        self.diffs = vec![CheckoutDiff {
            checkout_id: "checkout".into(),
            device_id: "local".into(),
            cwd: cwd.into(),
            patch: patch.into(),
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            truncated: false,
            checksum: format!("sum-{}", patch.len()),
            updated_at: chrono::Utc::now(),
        }];
        self.sync(cx);
    }

    /// slopty: tests: whether the find is open with the keyboard in it.
    #[cfg(test)]
    pub(crate) fn find_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.find.as_ref().is_some_and(|find| find.input.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Reconcile parsed content with the currently-active diff.
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.discard_stale_draft(cx);
        // The watch follows the selected chat's host device (idempotent when
        // the target is unchanged); a boot-deferred attempt retries here too.
        self.ensure_watch(cx);
        self.ensure_branches(cx); // slopty: no History surface (ruled out)
        self.ensure_scoped(cx);
        let Some(diff) = self.active_diff(cx) else {
            if self.parsed.take().is_some() {
                self.rows.clear();
                self.row_ranges.clear();
                self.list.reset(0);
                self.folds.clear();
                self.highlights.clear();
                self.sources.clear();
                self.words.clear();
                cx.notify();
            }
            return;
        };
        let key = self.parse_key(&diff);
        if self.parsed.as_ref().is_some_and(|p| p.key == key) {
            self.sync_comment_rows(cx);
            return;
        }
        // Parse off the render path — patches run to megabytes.
        let patch = diff.patch.clone();
        let truncated = diff.truncated;
        let additions = diff.additions;
        let deletions = diff.deletions;
        let file_count = diff.files.len();
        let ignore_whitespace = self.ignore_whitespace; // slopty: whitespace ignored.
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    let mut files = parse_patch(&patch);
                    if ignore_whitespace {
                        review::ignore_whitespace(&mut files);
                    }
                    files
                })
                .await;
            // slopty: what is left once whitespace is ignored is counted again.
            let (additions, deletions, file_count) = if ignore_whitespace {
                (
                    files.iter().map(|f| f.additions).sum(),
                    files.iter().map(|f| f.deletions).sum(),
                    files.len(),
                )
            } else {
                (additions, deletions, file_count)
            };
            this.update(cx, |changes, cx| {
                // Late results for a superseded diff are re-checked by key.
                let current = changes.active_diff(cx).map(|d| changes.parse_key(&d));
                if current.as_deref() != Some(key.as_str()) {
                    return;
                }
                let file_count = if file_count > 0 {
                    file_count
                } else {
                    files.len()
                };
                let horizontal = files.iter().map(FileHorizontalState::new).collect();
                changes.folds.clear();
                changes.highlights.clear();
                changes.sources.clear();
                changes.words.clear();
                // slopty: a large diff opens with files past the budget collapsed.
                let held = held_back(&files);
                for (file, _) in files.iter().zip(&held).filter(|(_, held)| **held) {
                    changes.folds.insert(
                        file.path.clone(),
                        FileFold { collapsed: true, held: true, ..FileFold::default() },
                    );
                }
                let staged = changes.staged_comments(cx);
                let draft = changes.draft_anchor();
                let (rows, ranges) = flatten_rows(
                    &files,
                    &staged,
                    draft
                        .as_ref()
                        .map(|(path, side, line)| (path.as_str(), *side, *line)),
                    changes.mode,
                    |ix| held.get(ix).copied().unwrap_or(false),
                );
                changes.comment_key = comment_state_key(&staged, draft.as_ref());
                // The uniform hint keeps offsets for never-rendered rows
                // sane (most rows ARE lines); real heights land as rows
                // render.
                let row_height = px(diff_line_height(Theme::of(cx)));
                changes
                    .list
                    .reset_with_uniform_height(rows.len(), row_height);
                changes.rows = rows;
                changes.row_ranges = ranges;
                changes.parsed = Some(ParsedDiff {
                    key,
                    truncated,
                    additions,
                    deletions,
                    file_count,
                    horizontal,
                    files: Arc::new(files),
                });
                changes.refresh_find(cx); // slopty: the find follows the content.
                cx.notify();
            })
            .ok();
        }));
    }

    /// Swap one file's body rows (everything after its header) for
    /// `new_body`, splicing both the row model and the list state. gpui's
    /// `splice` shifts the logical scroll anchor by the count delta, so
    /// content below the fold stays put.
    fn replace_file_body(&mut self, file_ix: usize, new_body: Vec<DiffRow>) {
        let Some(range) = self.row_ranges.get(file_ix).cloned() else {
            return;
        };
        let body = range.start + 1..range.end;
        let delta = new_body.len() as isize - body.len() as isize;
        // Only splice the rows that moved: `ListState::splice` clamps the
        // scroll anchor to the range start when the anchored row is inside it,
        // so replacing a whole body jumped the pane to the top of the file.
        // slopty: a span past the rows (a stale range) splices nothing rather than panicking.
        let (prefix, suffix) = {
            let Some(old) = self.rows.get(body.clone()) else {
                return;
            };
            let prefix = old
                .iter()
                .zip(&new_body)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new_body[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            (prefix, suffix)
        };
        if delta == 0 && prefix + suffix >= body.len() {
            return;
        }
        let changed = body.start + prefix..body.end - suffix;
        let mid: Vec<DiffRow> = new_body[prefix..new_body.len() - suffix].to_vec();
        self.list.splice(changed.clone(), mid.len());
        self.rows.splice(changed, mid);
        self.row_ranges[file_ix] = range.start..(range.end as isize + delta) as usize;
        for r in &mut self.row_ranges[file_ix + 1..] {
            *r = (r.start as isize + delta) as usize..(r.end as isize + delta) as usize;
        }
    }

    fn toggle_fold(&mut self, file_ix: usize, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let Some(file) = parsed.files.get(file_ix) else {
            return;
        };
        if self.wrap_lines {
            let collapsed = !self
                .folds
                .get(&file.path)
                .is_some_and(|fold| fold.collapsed);
            let body = if collapsed {
                Vec::new()
            } else {
                body_rows(
                    file_ix as u32,
                    file,
                    &self.comments_for(&file.path, cx),
                    self.draft_anchor_in(&file.path),
                    self.mode,
                )
            };
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = collapsed;
            fold.held = false; // slopty: loaded
            fold.toggled_at = None;
            self.replace_file_body(file_ix, body);
            cx.notify();
            return;
        }
        let expanded_height = body_height_with(
            file,
            &self.comments_for(&file.path, cx),
            self.draft_anchor_in(&file.path),
            self.mode,
            diff_line_height(Theme::of(cx)),
        );
        let fold = self.folds.entry(file.path.clone()).or_default();
        let currently_collapsed = fold.collapsed;
        fold.from = if currently_collapsed {
            0.0
        } else {
            expanded_height
        };
        fold.to = if currently_collapsed {
            expanded_height
        } else {
            0.0
        };
        fold.collapsed = !currently_collapsed;
        fold.held = false; // slopty: loaded
        fold.epoch += 1;
        fold.toggled_at = Some(crate::clock::now(cx));
        // The body tweens as ONE clipped stand-in row; the settle sweep
        // swaps it for steady rows (all lines, or none) once the window
        // elapses.
        self.replace_file_body(
            file_ix,
            vec![DiffRow::FoldingBody {
                file: file_ix as u32,
            }],
        );
        self.ensure_fold_settle(cx);
    }

    /// Keep a sweep alive while any [`DiffRow::FoldingBody`] stand-ins
    /// remain; each tick settles the ones whose tween window has elapsed.
    fn ensure_fold_settle(&mut self, cx: &mut Context<Self>) {
        if self.fold_settle.is_some() {
            return;
        }
        self.fold_settle = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOLD_TWEEN_WINDOW).await;
                let more = this
                    .update(cx, |changes, cx| changes.settle_folds(cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
            this.update(cx, |changes, _| changes.fold_settle = None)
                .ok();
        }));
    }

    /// Replace every settled folding stand-in with its steady-state rows.
    /// Returns whether any stand-ins are still mid-tween.
    fn settle_folds(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        let files = parsed.files.clone();
        let mut pending = false;
        for file_ix in (0..self.row_ranges.len()).rev() {
            let range = &self.row_ranges[file_ix];
            let folding = self.rows.get(range.start + 1)
                == Some(&DiffRow::FoldingBody {
                    file: file_ix as u32,
                });
            if !folding {
                continue;
            }
            let Some(file) = files.get(file_ix) else {
                continue;
            };
            let fold = self.folds.get(&file.path).copied().unwrap_or_default();
            if fold.animating(crate::clock::now(cx)) {
                pending = true;
                continue;
            }
            let body = if fold.collapsed {
                Vec::new()
            } else {
                body_rows(
                    file_ix as u32,
                    file,
                    &self.comments_for(&file.path, cx),
                    self.draft_anchor_in(&file.path),
                    self.mode,
                )
            };
            self.replace_file_body(file_ix, body);
        }
        cx.notify();
        pending
    }

    /// Every parsed file currently folded shut?
    fn all_collapsed(&self) -> bool {
        let Some(parsed) = &self.parsed else {
            return false;
        };
        !parsed.files.is_empty()
            && parsed.files.iter().all(|file| {
                self.folds
                    .get(&file.path)
                    .is_some_and(|fold| fold.collapsed)
            })
    }

    fn fold_all_label(&self) -> &'static str {
        if self.all_collapsed() {
            "Expand all files"
        } else {
            "Collapse all files"
        }
    }

    /// Collapse every file section, or expand them all when everything is
    /// already shut (the toolbar's fold button, t3code parity). Steady-state
    /// writes — no per-row tween arming, the whole list just snaps. List
    /// splices run bottom-up over the OLD ranges (each is O(log n)), then
    /// the row model rebuilds wholesale; the scroll anchor rides the
    /// splices, landing on the nearest file header when its body vanishes.
    fn toggle_collapse_all(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let collapse = !self.all_collapsed();
        let files = parsed.files.clone();
        for file in files.iter() {
            let fold = self.folds.entry(file.path.clone()).or_default();
            fold.collapsed = collapse;
            fold.held &= collapse; // slopty: expanding all loads every file
            fold.toggled_at = None;
        }
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        for file_ix in (0..self.row_ranges.len().min(files.len())).rev() {
            let range = &self.row_ranges[file_ix];
            let body = range.start + 1..range.end;
            let new_len = if collapse {
                0
            } else {
                let file = &files[file_ix];
                let comments: Vec<ReviewComment> = staged
                    .iter()
                    .filter(|comment| !comment.is_file() && comment.path == file.path)
                    .cloned()
                    .collect();
                body_rows(
                    file_ix as u32,
                    file,
                    &comments,
                    self.draft_anchor_in(&file.path),
                    self.mode,
                )
                .len()
            };
            if body.len() != new_len {
                self.list.splice(body, new_len);
            }
        }
        let (rows, ranges) = flatten_rows(
            &files,
            &staged,
            draft
                .as_ref()
                .map(|(path, side, line)| (path.as_str(), *side, *line)),
            self.mode,
            |_| collapse,
        );
        self.rows = rows;
        self.row_ranges = ranges;
        cx.notify();
    }

    /// Swap unified ⇄ split (toolbar toggle). The parse is untouched — only
    /// the flattening changes — so this rebuilds the row model and re-anchors
    /// the scroll onto whichever file was under the viewport's top edge (row
    /// indices do not survive the re-pairing).
    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.mode = self.mode.toggled();
        self.reset_horizontal_scroll();
        let split = self.mode.is_split();
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.diff_split = split;
        });
        // A draft's `+` sits in a column that may not exist after the swap.
        self.hover = None;
        self.reflatten(cx);
    }

    fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap_lines = !self.wrap_lines;
        self.reset_horizontal_scroll();
        let wrap = self.wrap_lines;
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.diff_wrap = wrap;
        });

        // A folding stand-in has an analytic fixed-line height. Settle it
        // before switching to variable-height rows; steady rows can then be
        // measured by the virtual list at the current pane width.
        let folding = self
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::FoldingBody { .. }));
        if folding {
            self.fold_settle = None;
            for fold in self.folds.values_mut() {
                fold.toggled_at = None;
            }
            self.reflatten(cx);
        } else {
            self.list.remeasure();
            cx.notify();
        }
    }

    fn reflatten(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed else {
            cx.notify();
            return;
        };
        let files = parsed.files.clone();
        let top = self.list.logical_scroll_top().item_ix;
        let anchor_file = self
            .row_ranges
            .iter()
            .position(|range| range.contains(&top));
        let collapsed: Vec<bool> = files
            .iter()
            .map(|file| {
                self.folds
                    .get(&file.path)
                    .is_some_and(|fold| fold.collapsed)
            })
            .collect();
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        let (rows, ranges) = flatten_rows(
            &files,
            &staged,
            draft
                .as_ref()
                .map(|(path, side, line)| (path.as_str(), *side, *line)),
            self.mode,
            |ix| collapsed.get(ix).copied().unwrap_or(false),
        );
        let row_height = px(diff_line_height(Theme::of(cx)));
        self.list.reset_with_uniform_height(rows.len(), row_height);
        self.rows = rows;
        self.row_ranges = ranges;
        if let Some(start) = anchor_file
            .and_then(|ix| self.row_ranges.get(ix))
            .map(|r| r.start)
        {
            self.list.scroll_to_reveal_item(start);
        }
        cx.notify();
    }

    /// Cloned because rendering borrows `self` mutably a moment later.
    fn staged_comments(&self, cx: &App) -> Vec<ReviewComment> {
        let state = self.state.read(cx);
        state
            .review_comments(&state.composer_key())
            .iter()
            .filter(|comment| {
                self.draft
                    .as_ref()
                    .and_then(|draft| draft.editing_id.as_ref())
                    != Some(&comment.id)
            })
            .cloned()
            .collect()
    }

    fn comments_for(&self, path: &str, cx: &App) -> Vec<ReviewComment> {
        self.staged_comments(cx)
            .into_iter()
            .filter(|comment| !comment.is_file() && comment.path == path)
            .collect()
    }

    /// The parsed diff's pre-rename path for `path`, when the file moved.
    fn old_path_of(&self, path: &str) -> Option<String> {
        self.parsed
            .as_ref()?
            .files
            .iter()
            .find(|file| file.path == path)?
            .old_path
            .clone()
    }

    /// A draft belongs to the checkout it was opened over. Chat navigation
    /// swaps both the diff under it and the composer it would stage onto, so
    /// the half-written note is dropped rather than following the user across.
    fn discard_stale_draft(&mut self, cx: &mut Context<Self>) {
        let key = self.state.read(cx).composer_key();
        if self.draft.as_ref().is_some_and(|draft| draft.key != key) {
            self.draft = None;
            self.sync_comment_rows(cx);
            cx.notify();
        }
    }

    fn draft_anchor(&self) -> Option<(String, CommentSide, u32)> {
        self.draft
            .as_ref()
            .map(|draft| (draft.path.clone(), draft.side, draft.line))
    }

    fn draft_anchor_in(&self, path: &str) -> Option<(CommentSide, u32)> {
        self.draft
            .as_ref()
            .filter(|draft| draft.path == path)
            .map(|draft| (draft.side, draft.line))
    }

    fn sync_comment_rows(&mut self, cx: &mut Context<Self>) {
        if self.parsed.is_none() {
            return;
        }
        let staged = self.staged_comments(cx);
        let draft = self.draft_anchor();
        let key = comment_state_key(&staged, draft.as_ref());
        if key == self.comment_key {
            return;
        }
        self.comment_key = key;
        let Some(parsed) = &self.parsed else {
            return;
        };
        let files = parsed.files.clone();
        for file_ix in (0..self.row_ranges.len().min(files.len())).rev() {
            let file = &files[file_ix];
            // A mid-tween stand-in is the settle sweep's to replace.
            if self
                .folds
                .get(&file.path)
                .is_some_and(|fold| fold.collapsed)
            {
                continue;
            }
            let range = &self.row_ranges[file_ix];
            if self.rows.get(range.start + 1)
                == Some(&DiffRow::FoldingBody {
                    file: file_ix as u32,
                })
            {
                continue;
            }
            let comments: Vec<ReviewComment> = staged
                .iter()
                .filter(|comment| !comment.is_file() && comment.path == file.path)
                .cloned()
                .collect();
            let body = body_rows(
                file_ix as u32,
                file,
                &comments,
                self.draft_anchor_in(&file.path),
                self.mode,
            );
            self.replace_file_body(file_ix, body);
        }
        cx.notify();
    }

    fn set_hover(
        &mut self,
        path: &str,
        anchor: Option<(CommentSide, u32)>,
        cx: &mut Context<Self>,
    ) {
        let next = anchor.map(|(side, line)| HoverRow {
            path: path.to_string(),
            side,
            line,
        });
        if next != self.hover {
            self.hover = next;
            cx.notify();
        }
    }

    fn hovering(&self, path: &str, anchor: (CommentSide, u32)) -> bool {
        self.hover
            .as_ref()
            .is_some_and(|hover| hover.path == path && (hover.side, hover.line) == anchor)
    }

    fn clear_hover_at(&mut self, path: &str, anchor: (CommentSide, u32), cx: &mut Context<Self>) {
        if self.hovering(path, anchor) {
            self.hover = None;
            cx.notify();
        }
    }

    fn open_draft(
        &mut self,
        path: String,
        side: CommentSide,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| ComposerInput::new("Request a change…", cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.commit_draft(cx),
            ComposerInputEvent::Edited => cx.notify(),
            _ => {}
        });
        let handle = input.read(cx).focus_handle(cx);
        let key = self.state.read(cx).composer_key();
        let old_path = self.old_path_of(&path);
        self.draft = Some(CommentDraft {
            editing_id: None,
            key,
            path,
            old_path,
            side,
            line,
            input,
            _events: events,
        });
        window.focus(&handle, cx);
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn edit_comment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(comment) = state
            .review_comments(&state.composer_key())
            .iter()
            .find(|comment| comment.id == id && !comment.is_file())
            .cloned()
        else {
            return;
        };
        let Some((side, line)) = comment.diff_anchor() else {
            return;
        };
        self.open_draft(comment.path.clone(), side, line, window, cx);
        // slopty: no unwrap.
        let Some(draft) = self.draft.as_mut() else {
            return;
        };
        draft.editing_id = Some(comment.id);
        if let comments::CommentSource::Diff { old_path, .. } = comment.source {
            draft.old_path = old_path;
        }
        draft
            .input
            .update(cx, |input, cx| input.set_text(comment.body, cx));
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn commit_draft(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let body = draft.input.read(cx).text().trim().to_string();
        if body.is_empty() {
            self.sync_comment_rows(cx);
            cx.notify();
            return;
        }

        // `draft.key`, not the live one: the note stages onto the composer it
        // was written against even if the selection moved under it.
        let key = draft.key;
        self.state.update(cx, |state, cx| {
            if let Some(id) = draft.editing_id {
                state.update_review_comment_body(&key, &id, body);
            } else {
                let comment = ReviewComment::new(draft.path, draft.side, draft.line, body)
                    .renamed_from(draft.old_path);
                state.add_review_comment(&key, comment);
            }
            cx.notify();
        });
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn remove_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let key = state.composer_key();
            state.remove_review_comment(&key, id);
            cx.notify();
        });
        self.sync_comment_rows(cx);
        cx.notify();
    }

    fn close_scope_menu(&mut self, cx: &mut Context<Self>) {
        if self.scope_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.scope_menu);
        }
    }

    fn close_ref_menu(&mut self, cx: &mut Context<Self>) {
        if self.ref_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.ref_menu);
        }
    }

    /// Handle Escape before focused descendants such as a terminal receive it.
    /// A popup in its exit animation remains a blocker until it unmounts.
    pub(crate) fn handle_escape(&mut self, cx: &mut Context<Self>) -> bool {
        // slopty: the turn menu and the find bar close on Escape too.
        if self.turn_menu.is_open() {
            self.close_turn_menu(cx);
            return true;
        }
        if self.find.is_some() {
            self.close_find(cx);
            return true;
        }
        if self.ref_menu.is_open() {
            self.close_ref_menu(cx);
            return true;
        }
        if self.scope_menu.is_open() {
            self.close_scope_menu(cx);
            return true;
        }
        self.scope_menu.get().is_some()
            || self.ref_menu.get().is_some()
            || self.turn_menu.get().is_some() // slopty: the turn menu.
    }

    fn open_ref_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // "PaletteSearch" context: ↑↓/⏎ stay unbound in the input and bubble
        // to the card's key handler.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search branches…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.ref_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        let handle = search.read(cx).focus_handle(cx);
        // The highlight starts ON the current base (query is empty, so the
        // filtered rows are just the branch list).
        let active = self
            .base_ref
            .as_ref()
            .and_then(|base| self.branches.iter().position(|b| b == base))
            .unwrap_or(0);
        self.ref_menu.open(RefMenu {
            search,
            active,
            focus: cx.focus_handle(),
            _search_events: search_events,
        });
        self.ref_menu_scroll.reset();
        // Focusable before first paint (the add-space palette's proven order).
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Filtered branch indices for the open ref menu (ranked substring match).
    fn ref_menu_rows(&self, cx: &App) -> Vec<usize> {
        let query = self
            .ref_menu
            .get()
            .map(|menu| menu.search.read(cx).text().to_string())
            .unwrap_or_default();
        popover::filter_indices(&query, &self.branches)
    }

    /// Dropdown keys (bubbling from the focused search input): ↑↓ navigate,
    /// ⏎ picks the highlighted branch, Esc closes.
    fn ref_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // The card stays mounted (and focused) through the exit animation —
        // keys must not drive a dying menu.
        if !self.ref_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_ref_menu(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.ref_menu_rows(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.ref_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    self.ref_menu_scroll.scroll.scroll_to_item(menu.active);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.ref_menu.get().map(|m| m.active).unwrap_or(0);
                let pick = self
                    .ref_menu_rows(cx)
                    .get(active)
                    .and_then(|ix| self.branches.get(*ix).cloned());
                if let Some(branch) = pick {
                    self.set_base_ref(branch, cx);
                    self.close_ref_menu(cx);
                }
            }
            _ => {}
        }
    }

    /// Both sides of `path` whole, as the shown diff named them (`GET_CHECKOUT_FILE_DIFF_TEXT`);
    /// `None` with no diff or no engine.
    fn file_texts(
        &self,
        path: String,
        cx: &mut Context<Self>,
    ) -> Option<Task<Option<slopty_view_proto::CheckoutFileDiffText>>> {
        let diff = self.active_diff(cx)?;
        let engine = self.state.read(cx).engine().cloned()?;
        let target = self.desired_target(cx);
        let chat_id = self
            .state
            .read(cx)
            .selected_chat_row()
            .map(|chat| chat.id.clone());
        let request = slopty_view_proto::GetCheckoutFileDiffTextRequest {
            checkout_id: diff.checkout_id,
            cwd: diff.cwd,
            path,
            mode: self.scope.mode().to_string(),
            base_ref: self.base_ref.clone(),
            chat_id,
            commit_sha: None, // slopty: no History surface (ruled out)
            diff_checksum: diff.checksum,
        };
        let mut params = serde_json::to_value(request)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if let Some(target) = target {
            params.insert("targetDeviceId".into(), serde_json::Value::String(target));
        }
        Some(cx.spawn(async move |_, _| {
            engine
                .client()
                .call(methods::GET_CHECKOUT_FILE_DIFF_TEXT, serde_json::Value::Object(params))
                .await
                .ok()
                .and_then(|value| serde_json::from_value(value).ok())
        }))
    }

    /// slopty: show the unchanged lines above hunk `hunk_ix` of file `file_ix`: the next
    /// [`UNCHANGED_STEP`] up, or all of them, fetching the file's new side once (item 11).
    fn show_unchanged(
        &mut self,
        file_ix: usize,
        hunk_ix: usize,
        all: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(parsed) = &self.parsed else {
            return;
        };
        let Some(path) = parsed.files.get(file_ix).map(|file| file.path.clone()) else {
            return;
        };
        if let Some(source) = self.sources.get(&path).cloned() {
            self.reveal(file_ix, hunk_ix, all, &source, cx);
            return;
        }
        let key = parsed.key.clone();
        let Some(texts) = self.file_texts(path.clone(), cx) else {
            return;
        };
        self.source_task = Some(cx.spawn(async move |this, cx| {
            let Some(response) = texts.await else {
                return;
            };
            this.update(cx, |changes, cx| {
                let Some(parsed) = changes.parsed.as_ref().filter(|parsed| parsed.key == key) else {
                    return;
                };
                let Some(file) = parsed.files.get(file_ix).filter(|file| file.path == path) else {
                    return;
                };
                let Some(source) = new_side_lines(file, &response) else {
                    return;
                };
                changes.sources.insert(path, source.clone());
                changes.reveal(file_ix, hunk_ix, all, &source, cx);
            })
            .ok();
        }));
    }

    /// Reveal the lines in the parsed file and rebuild what hangs off them: the file's width,
    /// its rows (unless folded) and the find.
    fn reveal(
        &mut self,
        file_ix: usize,
        hunk_ix: usize,
        all: bool,
        source: &[String],
        cx: &mut Context<Self>,
    ) {
        let most = if all { u32::MAX } else { UNCHANGED_STEP };
        let Some(parsed) = self.parsed.as_mut() else {
            return;
        };
        let Some(file) = Arc::make_mut(&mut parsed.files).get_mut(file_ix) else {
            return;
        };
        if !reveal_unchanged(file, hunk_ix, most, source) {
            return;
        }
        self.words.remove(&(file_ix as u32, hunk_ix as u32));
        let file = file.clone();
        if let Some(horizontal) = parsed.horizontal.get_mut(file_ix) {
            horizontal.geometry = DiffHorizontalGeometry::from_file(&file);
            horizontal.measured.replace(None);
        }
        let folded = self.folds.get(&file.path).is_some_and(|fold| fold.collapsed);
        let tweening = self.row_ranges.get(file_ix).and_then(|range| self.rows.get(range.start + 1))
            == Some(&DiffRow::FoldingBody { file: file_ix as u32 });
        if !folded && !tweening {
            let body = body_rows(
                file_ix as u32,
                &file,
                &self.comments_for(&file.path, cx),
                self.draft_anchor_in(&file.path),
                self.mode,
            );
            self.replace_file_body(file_ix, body);
        }
        self.refresh_find(cx);
        cx.notify();
    }

    /// slopty: the find's marks on line `line` of hunk `hunk` of file `file`, then the words it
    /// changed, lit in its own hue (22 % on dark, 16 % on light; design-inspiration item 14),
    /// worked out once per hunk.
    fn line_marks(
        &mut self,
        file: u32,
        hunk: u32,
        line: u32,
        theme: &Theme,
    ) -> Vec<(std::ops::Range<usize>, gpui::Hsla)> {
        let mut marks = self.find_marks(file, hunk, line, theme);
        let words = match self.words.get(&(file, hunk)) {
            Some(words) => words.clone(),
            None => {
                let Some(found) = self.parsed.as_ref().and_then(|parsed| {
                    parsed.files.get(file as usize)?.hunks.get(hunk as usize)
                }) else {
                    return marks;
                };
                let words = Arc::new(words::hunk_words(found));
                self.words.insert((file, hunk), words.clone());
                words
            }
        };
        let Some(spans) = words.get(line as usize).filter(|spans| !spans.is_empty()) else {
            return marks;
        };
        let kind = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(file as usize)?.hunks.get(hunk as usize)?.lines.get(line as usize))
            .map(|line| line.kind);
        let hue = match kind {
            Some(LineKind::Add) => add_color(theme),
            Some(LineKind::Del) => del_color(theme),
            _ => return marks,
        };
        let lit = hue.opacity(if theme.appearance.is_dark() { 0.22 } else { 0.16 });
        // The find's marks come first, so a match inside a changed word shows as a match.
        marks.extend(spans.iter().map(|span| (span.clone(), lit)));
        marks
    }

    /// Start excerpt parsing and a lazy full-source fetch for an expanded file.
    fn request_highlight(
        &mut self,
        file: &FileDiff,
        parsed_key: &str,
        cx: &mut Context<Self>,
    ) -> Option<Arc<DiffHighlights>> {
        let lang = slopty_syntax::language_for_path(&file.path)?;
        let fingerprint = hash64(&[parsed_key, &file.path]);
        if let Some(slot) = self.highlights.get(&file.path)
            && slot.fingerprint == fingerprint
        {
            return match &slot.state {
                DiffHighlightState::Ready(highlights) | DiffHighlightState::Excerpt(highlights) => {
                    Some(highlights.clone())
                }
                DiffHighlightState::Pending | DiffHighlightState::Plain => None,
            };
        }
        if !slopty_syntax::supports_language(lang) {
            self.highlights.insert(
                file.path.clone(),
                HighlightSlot {
                    fingerprint,
                    state: DiffHighlightState::Plain,
                    _excerpt_task: None,
                    _fetch_task: None,
                },
            );
            return None;
        }
        let path = file.path.clone();
        let excerpt_file = file.clone();
        let excerpt_path = path.clone();
        let excerpt_task = cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move { excerpt_highlights(&excerpt_file, lang).map(Arc::new) })
                .await;
            this.update(cx, |changes, cx| {
                if let Some(slot) = changes.highlights.get_mut(&excerpt_path)
                    && slot.fingerprint == fingerprint
                    && matches!(slot.state, DiffHighlightState::Pending)
                {
                    slot.state = match highlights {
                        Some(highlights) => DiffHighlightState::Excerpt(highlights),
                        None => DiffHighlightState::Plain,
                    };
                    cx.notify();
                }
            })
            .ok();
        });

        let fetch_file = file.clone();
        let fetch_path = path.clone();
        let fetch_task = self.file_texts(path.clone(), cx).map(|texts| {
            cx.spawn(async move |this, cx| {
                let response = texts.await;
                let (highlights, source) = match response {
                    Some(response) => {
                        cx.background_executor()
                            .spawn(async move {
                                // slopty: the new side, kept for the unchanged-lines rows.
                                let source = new_side_lines(&fetch_file, &response);
                                let highlights =
                                    full_highlights(&fetch_file, lang, &response).map(Arc::new);
                                (highlights, source)
                            })
                            .await
                    }
                    None => (None, None),
                };
                this.update(cx, |changes, cx| {
                    let current = changes.highlights.get(&fetch_path);
                    if let Some(source) = source
                        && current.is_some_and(|slot| slot.fingerprint == fingerprint)
                    {
                        changes.sources.insert(fetch_path.clone(), source);
                    }
                    if let Some(slot) = changes.highlights.get_mut(&fetch_path)
                        && slot.fingerprint == fingerprint
                        && let Some(highlights) = highlights
                    {
                        slot.state = DiffHighlightState::Ready(highlights);
                        cx.notify();
                    }
                })
                .ok();
            })
        });
        self.highlights.insert(
            file.path.clone(),
            HighlightSlot {
                fingerprint,
                state: DiffHighlightState::Pending,
                _excerpt_task: Some(excerpt_task),
                _fetch_task: fetch_task,
            },
        );
        None
    }

    // ---- rendering ----

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(parsed) = &self.parsed else {
            return gpui::Empty.into_any_element();
        };
        let files = parsed.files.clone();
        let parsed_key = parsed.key.clone();
        let Some(row) = self.rows.get(ix).copied() else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let highlight = files
            .get(row.file())
            .and_then(|file| self.request_highlight(file, &parsed_key, cx));
        // slopty: no unwrap or index: a row past the parse draws nothing.
        let Some(horizontal) = self.parsed.as_ref().and_then(|parsed| parsed.horizontal.get(row.file()))
        else {
            return gpui::Empty.into_any_element();
        };
        let code_width = match files.get(row.file()) {
            Some(file) if !self.wrap_lines => DiffCodeWidth::Scrollable(horizontal.metrics(
                file,
                highlight.as_ref(),
                &theme,
                window.text_system(),
                crate::typography::generation(cx),
            )),
            _ => DiffCodeWidth::Wrapped,
        };
        let code_scroll = DiffCodeScrollContext {
            handle: horizontal.scroll.clone(),
            prefix: SharedString::from(format!("changes-code-row-{ix}")),
        };
        match row {
            DiffRow::FileHeader { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                self.render_file_header(
                    file as usize,
                    file_diff,
                    &fold,
                    FileHeaderPresentation::Row,
                    &theme,
                    cx,
                )
            }
            DiffRow::Notice { file, notice } => files
                .get(file as usize)
                .and_then(|f| file_notices(f).into_iter().nth(notice as usize))
                .map(|text| notice_row(text, &theme))
                .unwrap_or_else(|| gpui::Empty.into_any_element()),
            DiffRow::Unchanged { file, hunk } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let Some(header) = file_diff.hunks.get(hunk as usize).map(|h| h.header.as_str())
                else {
                    return gpui::Empty.into_any_element();
                };
                let count = unchanged_before(file_diff, hunk as usize);
                let gutter_px = gutter_width(file_diff);
                let (lead, pad) = match self.mode {
                    DiffMode::Unified => {
                        (ACCENT_BAR_WIDTH + 2.0 * gutter_px, UNIFIED_CODE_PADDING_LEFT)
                    }
                    DiffMode::Split => (ACCENT_BAR_WIDTH + gutter_px, SPLIT_CODE_PADDING_LEFT),
                };
                // slopty: a click shows the next lines up, ⌥-click all of them (item 11).
                let hint = if count <= UNCHANGED_STEP {
                    "Show these lines".to_owned()
                } else {
                    format!("Show {UNCHANGED_STEP} more lines, ⌥-click for all {count}")
                };
                let (file_ix, hunk_ix) = (file as usize, hunk as usize);
                unchanged_row(count, header, lead, pad, true, &theme)
                    .id(("diff-unchanged", ix))
                    .group(UNCHANGED_GROUP)
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .hint(hint)
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        this.show_unchanged(file_ix, hunk_ix, event.modifiers().alt, cx);
                    }))
                    .into_any_element()
            }
            DiffRow::Line {
                file,
                hunk,
                line,
                flat: _,
            } => {
                let line_ix = line; // slopty: the find marks lines by index.
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let Some(line) = file_diff
                    .hunks
                    .get(hunk as usize)
                    .and_then(|h| h.lines.get(line as usize))
                else {
                    return gpui::Empty.into_any_element();
                };
                let spans = highlight
                    .as_deref()
                    .map(|highlights| highlights.spans(line))
                    .unwrap_or(&[]);
                let gutter_px = gutter_width(file_diff);
                let marks = self.line_marks(file, hunk, line_ix, &theme); // slopty: find, words.
                let row = diff_line_row(
                    line,
                    spans,
                    &theme,
                    gutter_px,
                    code_width,
                    Some(code_scroll.slot("unified")),
                    &marks,
                );
                let Some((side, line_no)) = line_anchor(line) else {
                    return row;
                };
                let path = file_diff.path.clone();
                let hovered = self.hovering(&path, (side, line_no));
                let move_path = path.clone();
                let leave_path = path.clone();
                div()
                    .id(("diff-line", ix))
                    .w_full()
                    .relative()
                    .child(row)
                    .when(hovered, |el| {
                        el.child(positioned_adder(
                            comment_adder_left(side, gutter_px),
                            render_comment_adder(&path, side, line_no, &theme, cx),
                        ))
                    })
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        this.set_hover(&move_path, Some((side, line_no)), cx);
                    }))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if !*hovered {
                            this.clear_hover_at(&leave_path, (side, line_no), cx);
                        }
                    }))
                    .into_any_element()
            }
            DiffRow::SplitLine {
                file,
                hunk,
                left,
                right,
            } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let Some(lines) = file_diff.hunks.get(hunk as usize).map(|h| &h.lines) else {
                    return gpui::Empty.into_any_element();
                };
                let gutter_px = gutter_width(file_diff);
                // Same slot on both sides = a context row: one line, drawn in
                // both columns.
                let mirrored = left.is_some() && left == right;
                // slopty: the find's marks on each column's line.
                let left_marks = left
                    .map(|slot| self.line_marks(file, hunk, slot, &theme))
                    .unwrap_or_default();
                let right_marks = right
                    .map(|slot| self.line_marks(file, hunk, slot, &theme))
                    .unwrap_or_default();
                let left = left.and_then(|slot| lines.get(slot as usize));
                let right = right.and_then(|slot| lines.get(slot as usize));
                // `\ No newline at end of file` is not code on one side — it
                // is a note about the row, so it spans both columns. Pairing
                // never puts a marker opposite code, so either side having one
                // means the whole row is the marker.
                if let Some(line) = [left, right]
                    .into_iter()
                    .flatten()
                    .find(|line| line.kind == LineKind::Meta)
                {
                    return meta_line_row(&line.text, &theme, 2.0 * (ACCENT_BAR_WIDTH + gutter_px));
                }
                // Refcounted, not cloned per listener: a split row wires up to
                // four of them, and this runs for every row in the viewport
                // plus the list's overdraw, every frame.
                let path: SharedString = file_diff.path.clone().into();
                // A mirrored row's columns carry the same text and the same
                // spans, so the runs are built once and shared — context is
                // most of a diff, so this is most of the rows.
                let shared_runs = mirrored
                    .then(|| left.map(|line| line_runs(line, highlight.as_deref(), &theme)))
                    .flatten();
                let cell = |line: Option<&DiffLine>, old: bool| {
                    line.map(|line| {
                        let runs = shared_runs
                            .clone()
                            .unwrap_or_else(|| line_runs(line, highlight.as_deref(), &theme));
                        // slopty: the find's marks.
                        let runs =
                            review::mark_runs(runs, if old { &left_marks } else { &right_marks });
                        let number = if old { line.old_no } else { line.new_no };
                        split_line_cell(
                            line,
                            number,
                            runs,
                            &theme,
                            gutter_px,
                            code_width,
                            Some(code_scroll.slot(if old { "old" } else { "new" })),
                        )
                    })
                };
                // The left column is inert. It shows the pre-change file, and
                // a deleted line is not there to be changed — a note on it
                // would cite a line the agent cannot edit. Everything is cited
                // against the new file, so only the right column takes a `+`.
                // Cards for old-side notes still render (they are pushed by
                // the row, not the column), so switching layouts never hides
                // one that is already staged.
                let left = cell(left, true)
                    .map(IntoElement::into_any_element)
                    .unwrap_or_else(|| split_filler().into_any_element());
                let right = match (cell(right, false), right.and_then(line_anchor)) {
                    (Some(cell), Some(anchor)) => {
                        let (side, line_no) = anchor;
                        let (move_path, leave_path) = (path.clone(), path.clone());
                        cell.id(("split-new", ix))
                            .when(self.hovering(&path, anchor), |el| {
                                el.relative().child(positioned_adder(
                                    split_adder_left(gutter_px),
                                    render_comment_adder(&path, side, line_no, &theme, cx),
                                ))
                            })
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                this.set_hover(&move_path, Some(anchor), cx);
                            }))
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                if !*hovered {
                                    this.clear_hover_at(&leave_path, anchor, cx);
                                }
                            }))
                            .into_any_element()
                    }
                    (Some(cell), None) => cell.into_any_element(),
                    (None, _) => split_filler().into_any_element(),
                };
                split_row(left, right, self.wrap_lines, &theme).into_any_element()
            }
            DiffRow::CommentCard { file, card } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let comments = self.comments_for(&file_diff.path, cx);
                match comments.get(card as usize) {
                    Some(comment) => crate::comment_ui::render_comment_card(
                        comment,
                        &theme,
                        cx,
                        Self::edit_comment,
                        Self::remove_comment,
                        None,
                    ),
                    None => gpui::Empty.into_any_element(),
                }
            }
            DiffRow::CommentDraft { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                match self
                    .draft
                    .as_ref()
                    .filter(|draft| draft.path == file_diff.path)
                {
                    // Header cites the same path the staged card and the
                    // prompt bullet will.
                    Some(draft) => crate::comment_ui::render_comment_draft(
                        draft_cite_path(draft),
                        draft.line,
                        draft.input.clone(),
                        draft.editing_id.is_some(),
                        &theme,
                        cx,
                        Self::cancel_draft,
                        Self::commit_draft,
                        None,
                    ),
                    None => gpui::Empty.into_any_element(),
                }
            }
            DiffRow::BodyPad { .. } => div().w_full().h(px(BODY_BOTTOM_PAD)).into_any_element(),
            DiffRow::FoldingBody { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                let (from, to) = (fold.from, fold.to);
                // Only the revealable slice is built — the tween never pays
                // for lines it cannot show.
                let cap = from.max(to).min(FOLD_TWEEN_MAX_PX);
                let body = render_file_body_upto(
                    file_diff,
                    highlight,
                    &theme,
                    cap,
                    self.mode,
                    code_width,
                    Some(DiffCodeScrollContext {
                        handle: code_scroll.handle.clone(),
                        prefix: SharedString::from(format!(
                            "changes-fold-code-{file}-{}",
                            fold.epoch
                        )),
                    }),
                );
                let clipped = div().w_full().overflow_hidden().child(body);
                if fold.animating(crate::clock::now(cx)) {
                    // slopty: the one fold (`motion::fold_frame`): the height opens on the quint
                    // while the lines wait 80 ms and fade in rising 3 px; closing, they fade
                    // before the height lands.
                    let opening = to > from;
                    let span = if opening {
                        motion::FOLD_OPEN.total().max(motion::FOLD_REVEAL.total())
                    } else {
                        motion::FOLD_CLOSE.total().max(motion::FOLD_HIDE.total())
                    }
                    .mul_f32(motion::speed_scale());
                    let still = motion::reduced_motion(cx);
                    let full = from.max(to);
                    clipped
                        .with_animation(
                            SharedString::from(format!("fold-{}-{}", file_diff.path, fold.epoch)),
                            gpui::Animation::new(span),
                            move |el, t| {
                                let frame = motion::fold_frame(opening, span.mul_f32(t), still);
                                el.h(px(full * frame.height))
                                    .pt(px(frame.rise))
                                    .opacity(frame.opacity)
                            },
                        )
                        .into_any_element()
                } else {
                    // Post-tween, pre-settle: hold the full target height so
                    // the settle splice swaps rows without any reflow (the
                    // capped slice always covers what the viewport can see —
                    // tweens start from a clicked, on-screen header).
                    clipped.h(px(to)).into_any_element()
                }
            }
        }
    }

    fn render_file_header(
        &mut self,
        ix: usize,
        file: &FileDiff,
        fold: &FileFold,
        presentation: FileHeaderPresentation,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = fold.collapsed;
        let path = file.path.clone();
        let adds = file.additions;
        let dels = file.deletions;
        let sticky = presentation == FileHeaderPresentation::Sticky;
        let sticky_paint = sticky.then(|| sticky_file_header_paint(theme));
        let rest_bg = if let Some(paint) = sticky_paint {
            paint.rest_bg
        } else {
            theme.ink(0.025)
        };
        let hover_bg = if let Some(paint) = sticky_paint {
            paint.hover_bg
        } else {
            theme.ink(0.05)
        };

        // Chevron (slopty checkout-diff-sidebar): chevron-right closed,
        // chevron-down open; gpui divs have no rotation transform at the
        // pinned rev, so the glyph swap crossfades over the same 200 ms.
        let chevron_icon = if collapsed {
            crate::icons::ALT_ARROW_RIGHT
        } else {
            crate::icons::ALT_ARROW_DOWN
        };
        // slopty: 12, as the scope trigger's chevron.
        let chevron = div().flex_none().size(px(14.0)).flex().items_center().justify_center().child(
            crate::icons::icon(chevron_icon)
                .size(px(12.0))
                .text_color(theme.text_muted.opacity(0.7)),
        );
        let chevron: AnyElement = if fold.animating(crate::clock::now(cx)) {
            chevron
                .with_animation(
                    SharedString::from(format!(
                        "chev-{}-{path}-{}",
                        presentation.key_prefix(),
                        fold.epoch
                    )),
                    CHEVRON.animation(),
                    |el, t| el.opacity(0.25 + 0.75 * t),
                )
                .into_any_element()
        } else {
            chevron.into_any_element()
        };

        // Header row: chevron + mono path (one quiet tone) + right-aligned
        // +N / −N counts on a slightly raised wash. The header carries the
        // section separator (the per-file wrapper it used to hang on is
        // gone — rows are flat now).
        // slopty: a header names its file and whether it is open; a file a large diff held back
        // says how many lines a click loads.
        let held_lines = (fold.held && collapsed).then(|| diff_lines(file));
        let header_fade = format!("{}-{ix}-hover", presentation.key_prefix());
        let header_label = match held_lines {
            Some(lines) => format!("{path}, {lines} lines not loaded. Load"),
            None if collapsed => format!("{path}, collapsed"),
            None => format!("{path}, expanded"),
        };
        div()
            .id(presentation.element_id(ix))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(header_label))
            // slopty: a keyboard stop with the shell's ring, inside the list (M8).
            .map(|el| crate::surface_chrome::key_stop_inset(el, theme))
            .w_full()
            .h(px(FILE_HEADER_HEIGHT))
            .when(
                presentation == FileHeaderPresentation::Row && ix > 0,
                |el| el.border_t_1().border_color(crate::theme::hairline(0.04)),
            )
            .when(sticky, |el| {
                el.border_b_1()
                    .border_color(sticky_paint.expect("sticky paint").border)
                    .block_mouse_except_scroll()
            })
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(Theme::SPACE_MD))
            // slopty: the hover wash fades in, as every row's (M23).
            .bg(motion::hover_blend(&header_fade, rest_bg, hover_bg))
            .hover_fade(header_fade.clone())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_fold(ix, cx);
                cx.notify();
            }))
            .child(chevron)
            .child(
                crate::file_icons::icon(
                    crate::file_icons::FileIconIdentity::file(&file.path),
                    theme.appearance,
                )
                .size(px(14.0))
                .flex_none(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(file.path.clone())),
            )
            .when(file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from("BIN")),
                )
            })
            // slopty: what a held file's click loads.
            .when_some(held_lines, |el, lines| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!("Large diff · {lines} lines · Load"))),
                )
            })
            // slopty: a count shows only when it is not zero (T3), in tabular figures; a
            // mode-only change keeps one quiet "0".
            .children(file_counts(adds, dels, file.binary).into_iter().map(|(text, kind)| {
                div()
                    .flex_none()
                    .font_family(theme.font_mono.clone())
                    .font_features(crate::typography::tabular())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(match kind {
                        CountKind::Added => add_color(theme),
                        CountKind::Deleted => del_color(theme),
                        CountKind::None => theme.text_faint,
                    })
                    .child(SharedString::from(text))
            }))
            .when(adds + dels > 0, |el| el.child(diffstat_blocks(adds, dels, theme)))
            .child(
                // slopty: the pane's icon button, its "opens elsewhere" glyph, a Button for a
                // screen reader.
                crate::surface_chrome::icon_button(
                    format!("diff-open-file-{}-{ix}", presentation.key_prefix()),
                    crate::icons::ARROW_UP_RIGHT,
                    theme,
                )
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(ChangesEvent::OpenFile(path.clone()));
                }))
                .hint("Open file"),
            )
            .into_any_element()
    }

    fn render_sticky_file_header(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let scroll_top = self.list.logical_scroll_top();
        let sticky = sticky_file_header(
            &self.row_ranges,
            scroll_top.item_ix,
            scroll_top.offset_in_item.as_f32(),
        )?;
        debug_assert_eq!(
            self.rows.get(sticky.header_row),
            Some(&DiffRow::FileHeader {
                file: sticky.file_ix as u32,
            })
        );
        let files = self.parsed.as_ref()?.files.clone();
        let file = files.get(sticky.file_ix)?;
        let fold = self.folds.get(&file.path).copied().unwrap_or_default();
        let next_header_y = sticky.next_header_row.and_then(|row| {
            let bounds = self.list.bounds_for_item(row)?;
            let viewport = self.list.viewport_bounds();
            Some((bounds.origin.y - viewport.origin.y).as_f32())
        });
        let top_offset = sticky_header_push_offset(next_header_y);
        let header = self.render_file_header(
            sticky.file_ix,
            file,
            &fold,
            FileHeaderPresentation::Sticky,
            theme,
            cx,
        );
        let paint = sticky_file_header_paint(theme);
        // The sticky floats over diff rows, but it belongs to the same content
        // plane. Tint the blur with `theme.bg`; `glass_overlay` is deliberately
        // reserved for elevated menus/cards and produced the wrong hue here.
        let header = if let Some(tint) = paint.frost_tint {
            div().w_full().bg(tint).child(header).into_any_element()
        } else {
            header
        };
        // Frosted is a pass-through when the resolved surface is opaque.
        let header = crate::frost::frosted(0.0, STICKY_FILE_HEADER_BLUR, header);

        Some(
            div()
                .absolute()
                .top(px(top_offset))
                .left_0()
                .w_full()
                .child(header)
                .into_any_element(),
        )
    }

    /// A small hover-washed icon button for the pane header. The header lives
    /// inside the titlebar drag strip, so the button occludes and swallows the
    /// mouse-down (same discipline as the shell's `header_icon_button`).
    // slopty: the pane's one icon button (`surface_chrome::icon_button`).
    fn header_button(
        id: &'static str,
        icon_path: &'static str,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        crate::surface_chrome::icon_button(id, icon_path, theme).occlude().hint(label)
    }

    /// [`Self::header_button`] for a toggle: latched while `active`, so the pane says which
    /// layout it is in without a label, and a screen reader hears it. The caller names it
    /// ([`crate::settings::widgets::ActionHint`]): its words, and its key when it has one.
    // slopty: the pane's one toggle (`surface_chrome::icon_toggle`): hover still answers a
    // latched one.
    fn header_toggle(
        id: &'static str,
        icon_path: &'static str,
        active: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        crate::surface_chrome::icon_toggle(id, icon_path, active, theme).occlude()
    }

    /// The unified ⇄ split layout toggle (both the scoped and the
    /// commit-pinned toolbars carry it).
    fn split_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle(
            "changes-split",
            crate::icons::SPLIT_COLUMNS,
            self.mode.is_split(),
            theme,
        )
        .hint("Split view")
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_mode(cx);
        }))
        .into_any_element()
    }

    fn wrap_toggle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        Self::header_toggle(
            "changes-wrap",
            crate::icons::WRAP_TEXT,
            self.wrap_lines,
            theme,
        )
        .hint("Wrap long lines")
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_wrap(cx);
        }))
        .into_any_element()
    }

    /// The pane-header controls: scope dropdown, `{branch} → {base ⌄}` ref
    /// selector (branch scope), fold-all. Rendered BY THE SHELL inside the
    /// session titlebar's trailing section (the band above the pane) — the
    /// titlebar overlay owns that strip's hit-testing, so controls mounted
    /// under it would never see a click. The expand and close buttons ride
    /// alongside, shell-owned (they mutate shell state).
    pub fn render_header_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        // slopty: no History surface (ruled out)
        let scope = self.scope;
        let scope_open = self.scope_menu.get().is_some();
        let scope_trigger = div()
            .id("changes-scope-trigger")
            // slopty: a screen reader hears what it picks and whether its menu is open (T3's
            // `Diff scope: …`).
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Diff scope: {}", scope.label())))
            .aria_expanded(scope_open)
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(8.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-scope-trigger",
                crate::theme::hover_wash(),
                crate::theme::control_hover(),
            ))
            .hover_fade("changes-scope-trigger")
            // slopty: held selected while its menu is open (opencode's `[data-expanded]`).
            .when(scope_open, |el| el.bg(crate::theme::selected_wash()))
            .active(|style| style.bg(crate::theme::pressed_wash()))
            // slopty: a keyboard stop with the shell's ring (M8).
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring(el, &theme))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.scope_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.scope_menu.take_press_was_open() {
                    this.close_scope_menu(cx);
                } else {
                    this.scope_menu.open(());
                }
                cx.notify();
            }))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text)
                    .child(SharedString::from(scope.label())),
            )
            .child(
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        // slopty: no History surface (ruled out)
        let trigger = if scope_open {
            let closing = self.scope_menu.closing_at(crate::clock::now(cx));
            let menu = self.render_scope_menu(&theme, cx);
            scope_trigger
                .relative()
                .child(popover::anchored_menu_below_gap(
                    "changes-scope-menu",
                    menu,
                    closing,
                    10.0,
                ))
                .into_any_element()
        } else {
            scope_trigger.into_any_element()
        };

        // slopty: no History surface (ruled out)
        let trailing: AnyElement = {
            // slopty: no Discard changes: the person reviews, agents fix (ruled out).
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(crate::surface_chrome::CONTROL_GAP))
                .children(self.render_aid_controls(&theme, cx)) // slopty: find, tree, whitespace.
                .child(self.split_toggle(&theme, cx))
                .child(self.wrap_toggle(&theme, cx))
                .child(
                    Self::header_button(
                        "changes-fold-all",
                        crate::icons::FOLD_VERTICAL,
                        self.fold_all_label(),
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_collapse_all(cx);
                    })),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::surface_chrome::CONTROL_GAP))
            .child(trigger)
            .children(self.render_ref_selector(&theme, cx))
            .children(self.render_turn_selector(&theme, cx)) // slopty: any turn's diff.
            .child(div().flex_1()) // slopty: no History surface (ruled out)
            .child(trailing)
            .into_any_element()
    }

    fn render_scope_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let current = self.scope;
        popover::popover_card(theme)
            .w(px(180.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_scope_menu(cx)))
            .child(
                // The 2px row gap every other menu carries — rows straight on
                // the card abutted, adjacent washes read as one slab (user
                // report).
                div().flex().flex_col().gap(px(2.0)).children(
                    DiffScope::ALL.into_iter().enumerate().map(|(ix, scope)| {
                        popover::menu_row(
                            theme,
                            scope == current,
                            format!("changes-scope-row-{ix}"),
                        )
                        .id(("changes-scope-row", ix))
                        // slopty: a keyboard stop with the shell's ring, inside the menu (M8).
                        .map(|el| crate::surface_chrome::key_stop_inset(el, theme))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_scope(scope, cx);
                            this.close_scope_menu(cx);
                        }))
                        .child(div().flex_1().child(SharedString::from(scope.label())))
                    }),
                ),
            )
            .into_any_element()
    }

    /// `{branch} → {base ⌄}` — which ref the branch scope compares against
    /// (t3code's ref strip), inlined into the pane header. Branch scope only.
    fn render_ref_selector(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.scope != DiffScope::Branch {
            return None;
        }
        let branch = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|chat| chat.branch.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let base = self.base_ref.clone().unwrap_or_else(|| "…".to_string());
        // Even truncation: taffy shrinks flex items ∝ factor × basis, and the
        // default factor of 1 splits the deficit proportionally to content —
        // a long branch stayed near-whole while a short base ("main") read as
        // a bare ellipsis (user report). Weighting each side's factor by its
        // own length SQUARED (mono font, so chars ∝ px) lands the deficit
        // ~cubically on the longer name: the short side's loss stays
        // sub-pixel even under a big deficit (a linear weight still cost it
        // a char), while equal lengths still split evenly.
        let branch_weight = (branch.chars().count().max(1) as f32).powi(2);
        let base_weight = (base.chars().count().max(1) as f32).powi(2);
        let trigger = div()
            .id("changes-ref-trigger")
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(6.0))
            // Shrinkable, like the branch label beside it — a flex_none
            // trigger with a long base name plowed over the header buttons
            // (user report); both sides truncate instead.
            .min_w_0()
            .flex_shrink(base_weight)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-ref-trigger",
                crate::theme::wash(0.0),
                crate::theme::control_hover(),
            ))
            .hover_fade("changes-ref-trigger")
            // slopty: held selected while its menu is open, and named as the menu it opens.
            .when(self.ref_menu.get().is_some(), |el| el.bg(crate::theme::selected_wash()))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Compare with {base}")))
            .aria_expanded(self.ref_menu.get().is_some())
            // slopty: a keyboard stop with the shell's ring (M8).
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.ref_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.ref_menu.take_press_was_open() {
                    this.close_ref_menu(cx);
                    cx.notify();
                } else {
                    this.open_ref_menu(window, cx);
                }
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.5))
                    .text_color(theme.text)
                    .child(SharedString::from(base)),
            )
            .child(
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let trigger = if self.ref_menu.get().is_some() {
            let closing = self.ref_menu.closing_at(crate::clock::now(cx));
            let menu = self.render_ref_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below_gap(
                "changes-ref-menu",
                menu,
                closing,
                10.0,
            ))
        } else {
            trigger
        };
        Some(
            div()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                // Extra room off the scope dropdown (row gap alone read
                // cramped — user report).
                .ml(px(crate::surface_chrome::CONTROL_GAP))
                .child(
                    div()
                        .min_w_0()
                        .flex_shrink(branch_weight)
                        .truncate()
                        .font_family(theme.font_mono.clone())
                        .text_size(px(11.5))
                        .text_color(theme.text_dim)
                        .child(SharedString::from(branch)),
                )
                .child(
                    crate::icons::icon(crate::icons::ARROW_RIGHT)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_faint),
                )
                .child(trigger)
                .into_any_element(),
        )
    }

    fn render_ref_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus) = {
            let Some(menu) = self.ref_menu.get() else {
                return div().into_any_element();
            };
            (menu.search.clone(), menu.active, menu.focus.clone())
        };
        let list_scroll = self.ref_menu_scroll.scroll.clone();
        let rows = self.ref_menu_rows(cx);
        let current = self.base_ref.clone();
        let branches = self.branches.clone();

        let list: AnyElement = if rows.is_empty() {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(if branches.is_empty() {
                    "No branches"
                } else {
                    "No matching branches"
                }))
                .into_any_element()
        } else {
            let rail = crate::settings::widgets::rail(
                &mut self.ref_menu_scroll,
                "changes-ref-scrollbar",
                theme,
                cx,
                |changes| &mut changes.ref_menu_scroll,
            );
            let list = popover::menu_scroll_list("changes-ref-list", &list_scroll)
                .flex()
                .flex_col()
                .gap(px(2.0))
                .max_h(px(240.0))
                .children(rows.into_iter().enumerate().map(|(row_ix, branch_ix)| {
                    let name = branches[branch_ix].clone();
                    let selected = current.as_deref() == Some(name.as_str());
                    let label = name.clone();
                    popover::picker_row(
                        theme,
                        selected,
                        row_ix == active,
                        format!("changes-ref-row-{row_ix}"),
                    )
                    .id(("changes-ref-row", row_ix))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_base_ref(name.clone(), cx);
                        this.close_ref_menu(cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.font_mono.clone())
                            .text_size(px(12.0))
                            .child(SharedString::from(label)),
                    )
                }));
            popover::menu_scroll_host("changes-ref-list-host")
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if this.ref_menu_scroll.set_list_hovered(*hovered) {
                        cx.notify();
                    }
                }))
                .child(popover::faded_menu_list(&list_scroll, list))
                .children(rail)
                .into_any_element()
        };

        popover::popover_card(theme)
            .w(px(240.0))
            .track_focus(&focus)
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| this.ref_menu_key(event, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_ref_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }

    fn render_header_strip(&self, theme: &Theme) -> Option<AnyElement> {
        let parsed = self.parsed.as_ref()?;
        Some(
            div()
                .flex_none()
                .h(px(crate::surface_chrome::HEADER_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .px(px(Theme::SPACE_LG))
                .border_b_1()
                .border_color(crate::theme::hairline(0.06))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(scope_label(
                            self.scope,
                            parsed.file_count,
                            self.base_ref.as_deref(),
                        ))),
                )
                // slopty: the totals in tabular figures, a zero total left out, as each file's.
                .children(
                    file_counts(parsed.additions, parsed.deletions, true).into_iter().map(
                        |(text, kind)| {
                            div()
                                .font_family(theme.font_mono.clone())
                                .font_features(crate::typography::tabular())
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(if kind == CountKind::Added {
                                    add_color(theme)
                                } else {
                                    del_color(theme)
                                })
                                .child(SharedString::from(text))
                        },
                    ),
                )
                .child(div().flex_1())
                // slopty: its tooltip says what is left out.
                .when(parsed.truncated, |el| {
                    el.child(
                        div()
                            .id("changes-partial")
                            .flex_none()
                            .text_size(crate::typography::ui_rems(11.0))
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .bg(theme.warning.opacity(0.08))
                            .text_color(theme.warning.opacity(0.75))
                            .hint("Too large to show whole: the diff shows its start")
                            .child(SharedString::from("Partial snapshot")),
                    )
                })
                .into_any_element(),
        )
    }
}

/// Green for additions — sampled from the reference diff (soft emerald).
pub(crate) fn add_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_add // emerald-400
}

/// Red for deletions — softer than the theme danger, per the reference diff.
pub(crate) fn del_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_del // red-400
}

/// One notice row ("New file", "Binary file — contents not shown", …).
pub(crate) fn notice_row(notice: String, theme: &Theme) -> AnyElement {
    div()
        .h(px(NOTICE_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(Theme::SPACE_LG))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_faint)
        .child(SharedString::from(notice))
        .into_any_element()
}

// slopty: what a diff leaves out between hunks reads as a count, "N unchanged lines", not the
// `@@` range (opencode's `line-info-basic` separator; opencode item 11), in muted italic, with the
// unfold mark and the function git names in the warning hue at 70 %, Amp's amber diff meta (Amp
// item 14): the colour sits in the ink on a neutral wash, as both draw it, not in a tinted band.
// The number columns take a stronger step of the wash.
/// The row standing for `count` unchanged lines above the hunk `header` opens. `lead` is the
/// width of the bar and number columns, `pad` the code's own left pad after them.
pub(crate) fn unchanged_row(
    count: u32,
    header: &str,
    lead: f32,
    pad: f32,
    expandable: bool,
    theme: &Theme,
) -> gpui::Div {
    let (_, context) = split_hunk_header(header);
    let meta = diff_meta_ink(theme);
    let label = if count == 1 {
        "1 unchanged line".to_owned()
    } else {
        format!("{count} unchanged lines")
    };
    div()
        .h(px(UNCHANGED_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .bg(theme.ink(0.04))
        .text_size(crate::typography::ui_rems(crate::typography::ladder::LABEL))
        .text_color(theme.text_muted)
        .whitespace_nowrap()
        .overflow_hidden()
        .child(
            div()
                .w(px(lead))
                .h_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.ink(0.04))
                .when(expandable, |el| {
                    el.child(
                        crate::icons::icon(crate::icons::UNFOLD_VERTICAL)
                            .size(px(crate::surface_chrome::ICON_SIZE))
                            .text_color(meta)
                            .group_hover(UNCHANGED_GROUP, |s| s.text_color(theme.warning)),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .pl(px(pad))
                .italic()
                .font_features(crate::typography::tabular())
                .when(expandable, |el| {
                    el.group_hover(UNCHANGED_GROUP, |s| s.text_color(theme.text))
                })
                .child(SharedString::from(label)),
        )
        .when(!context.is_empty(), |row| {
            row.child(
                div()
                    .min_w_0()
                    .pl(px(Theme::SPACE_SM))
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(11.0))
                    .text_color(meta)
                    .child(SharedString::from(context.to_owned())),
            )
        })
}

/// slopty: a diff's meta, its hunk's function and the unfold mark, in the warning hue at 70 %
/// (Amp's `#a07013` / `#d6a038`; Amp item 14): colour, not grey.
pub(crate) fn diff_meta_ink(theme: &Theme) -> gpui::Hsla {
    theme.warning.opacity(0.7)
}

/// The hover group of an unchanged-lines row the pane can open.
const UNCHANGED_GROUP: &str = "diff-unchanged";

/// A hunk header's range (`@@ -1,4 +1,5 @@`) and the function context git writes after it.
fn split_hunk_header(header: &str) -> (&str, &str) {
    header
        .strip_prefix("@@")
        .and_then(|rest| rest.find("@@"))
        .map_or((header.trim(), ""), |end| {
            let (range, context) = header.split_at(end + 4);
            (range.trim(), context.trim())
        })
}

/// The only part of a diff row allowed to exceed its viewport. The outer
/// element keeps row chrome fixed; the inner element owns the intrinsic code
/// width and is the only plane moved by the file's horizontal scroll handle.
fn code_text_viewport(
    text: String,
    runs: Vec<gpui::TextRun>,
    theme: &Theme,
    padding_left: f32,
    content_width: Option<f32>,
    wrapped: bool,
    scroll: Option<DiffCodeScroll>,
) -> AnyElement {
    let content = div()
        .when(wrapped, |el| el.w_full().min_w_0())
        .when_some(content_width, |el, width| {
            // Keep every tracked row's scroll extent identical. The width
            // already includes shaping slack on the right, so clipping here
            // only prevents a child from redefining the shared maximum.
            el.w(px(width)).flex_none().overflow_hidden()
        })
        .pl(px(padding_left))
        .font_family(theme.font_mono.clone())
        .text_size(px(diff_text_size(theme)))
        .line_height(px(diff_line_height(theme)))
        .map(|el| {
            if wrapped {
                el.whitespace_normal()
            } else {
                el.whitespace_nowrap()
            }
        })
        .child(gpui::StyledText::new(text).with_runs(runs));
    let viewport = div()
        .flex_1()
        .min_w_0()
        .min_h(px(diff_line_height(theme)))
        .overflow_hidden()
        .child(content);
    if wrapped {
        return viewport.into_any_element();
    }
    match scroll {
        Some(scroll) => {
            let mut viewport = viewport
                .id(scroll.id)
                .overflow_x_scroll()
                .track_scroll(&scroll.handle);
            // Without this GPUI maps a vertical-only wheel delta onto x for
            // an x-only scroller, starving the virtualized list underneath.
            viewport.style().restrict_scroll_to_axis = Some(true);
            // slopty: a line running past the pane fades out at the edge, and back in on the
            // left once scrolled, so the cut reads as "more" (T3's diff does; item 23). Each
            // ramp grows with what is hidden past it, so a line that fits never dims.
            crate::edge_fade::edge_faded(DIFF_EDGE_FADE, false, false, viewport)
                .fade_left(true)
                .fade_right(true)
                .fade_scroll_x(&scroll.handle)
                .into_any_element()
        }
        None => viewport.into_any_element(),
    }
}

// slopty: no pull-request pages.
/// One +/−/context/meta diff line: coloured accent bar, dual line-number
/// gutters (`gutter_px` wide — see [`gutter_width`]) and paint-only syntax
/// runs.
fn diff_line_row(
    line: &DiffLine,
    spans: &[slopty_syntax::HighlightSpan],
    theme: &Theme,
    gutter_px: f32,
    code_width: DiffCodeWidth,
    scroll: Option<DiffCodeScroll>,
    marks: &[(std::ops::Range<usize>, gpui::Hsla)], // slopty: the find's marks.
) -> AnyElement {
    if line.kind == LineKind::Meta {
        return meta_line_row(
            &line.text,
            theme,
            ACCENT_BAR_WIDTH + 2.0 * gutter_px + UNIFIED_CODE_PADDING_LEFT,
        );
    }

    // Row tints sampled from the reference: ~5–6% washes over the pane tone.
    let mut add_bg = add_color(theme);
    add_bg.a = 0.055;
    let mut del_bg = del_color(theme);
    del_bg.a = 0.055;

    let (row_bg, accent, number_color) = line_tones(line.kind, add_bg, del_bg, theme);
    let wrapped = matches!(code_width, DiffCodeWidth::Wrapped);
    let mono = font(theme.font_mono.clone());
    let runs = render::runs_for_syntax_line_with_plain(
        &line.text,
        spans,
        &mono,
        theme.text.opacity(0.92),
        theme,
    );
    let runs = review::mark_runs(runs, marks); // slopty: the find's marks.
    let content_width = match code_width {
        DiffCodeWidth::Clipped => None,
        DiffCodeWidth::Scrollable(metrics) => Some(metrics.unified_content_width(gutter_px)),
        DiffCodeWidth::Wrapped => None,
    };
    div()
        .map(|el| {
            if wrapped {
                el.min_h(px(diff_line_height(theme)))
            } else {
                el.h(px(diff_line_height(theme)))
            }
        })
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_start()
        .when_some(row_bg, |el, bg| el.bg(bg))
        // Accent bar: solid colour on +/− rows, invisible spacer on
        // context rows so columns always align.
        .child(
            div()
                .w(px(ACCENT_BAR_WIDTH))
                .self_stretch()
                .flex_none()
                .when_some(accent, |el, color| el.bg(color)),
        )
        .child(gutter_cell(
            line.old_no,
            if line.kind == LineKind::Del {
                number_color
            } else {
                theme.text_faint.opacity(0.8)
            },
            None,
            gutter_px,
            theme,
        ))
        .child(gutter_cell(
            line.new_no,
            if line.kind == LineKind::Add {
                number_color
            } else {
                theme.text_faint.opacity(0.8)
            },
            wrapped.then_some(line.text.as_str()),
            gutter_px,
            theme,
        ))
        .child(code_text_viewport(
            line.text.clone(),
            runs,
            theme,
            UNIFIED_CODE_PADDING_LEFT,
            content_width,
            wrapped,
            scroll,
        ))
        .into_any_element()
}

/// A line's number in its gutter. slopty: with wrap on, `wraps` is the line's text and each row
/// it wraps onto shows `»` under the number (AgentCraft's continuation mark; design-inspiration
/// item 13), so a wrapped line never reads as two.
fn gutter_cell(
    number: Option<u32>,
    color: gpui::Hsla,
    wraps: Option<&str>,
    gutter_px: f32,
    theme: &Theme,
) -> gpui::Div {
    let cell = div()
        .w(px(gutter_px))
        .flex_none()
        .font_family(theme.font_mono.clone())
        .text_size(px(11.0))
        .line_height(px(diff_line_height(theme)))
        .text_color(color)
        .flex()
        .justify_end()
        .pr(px(8.0));
    let number = SharedString::from(number.map(|n| n.to_string()).unwrap_or_default());
    match wraps.and_then(continuation_marks) {
        // The marks stand out of the layout, so the row stays as tall as its code, which clips
        // the ones it does not wrap onto.
        Some(marks) => cell.self_stretch().relative().overflow_hidden().child(
            div()
                .absolute()
                .top_0()
                .right(px(8.0))
                .flex()
                .flex_col()
                .items_end()
                .child(number)
                .child(div().text_color(theme.text_faint).child(marks)),
        ),
        None => cell.child(number),
    }
}

/// `»` for every row a line could wrap onto, one per line: at most one per 24 columns of the
/// line (no diff pane is narrower), so the row's own height clips the rest.
fn continuation_marks(text: &str) -> Option<SharedString> {
    let columns: usize = text
        .chars()
        .map(|ch| match ch {
            '\t' => 4,
            ch if ch.is_ascii() => 1,
            _ => 2,
        })
        .sum();
    let rows = (columns / 24).min(200);
    (rows > 0).then(|| vec!["»"; rows].join("\n").into())
}

/// A line's wash, its bar and its numbers' colour by kind. slopty: the bar takes the full hue, as
/// the only mark of an added or deleted line now the +/− column is gone (item 11).
fn line_tones(
    kind: LineKind,
    add_bg: gpui::Hsla,
    del_bg: gpui::Hsla,
    theme: &Theme,
) -> (Option<gpui::Hsla>, Option<gpui::Hsla>, gpui::Hsla) {
    match kind {
        LineKind::Add => (Some(add_bg), Some(add_color(theme)), add_color(theme).opacity(0.9)),
        LineKind::Del => (Some(del_bg), Some(del_color(theme)), del_color(theme).opacity(0.9)),
        _ => (None, None, theme.text_faint.opacity(0.8)),
    }
}

/// `\ No newline at end of file` and friends: a note about the row rather
/// than code, so it is indented past the columns and never tinted. In split
/// mode it spans both halves.
fn meta_line_row(text: &str, theme: &Theme, pad_left: f32) -> AnyElement {
    div()
        .h(px(diff_line_height(theme)))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .pl(px(pad_left))
        .text_size(px(11.0))
        .text_color(theme.text_faint)
        .italic()
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Split (side-by-side) rendering
// ---------------------------------------------------------------------------

/// The paint-only syntax runs for one diff line.
fn line_runs(
    line: &DiffLine,
    highlights: Option<&DiffHighlights>,
    theme: &Theme,
) -> Vec<gpui::TextRun> {
    let spans = highlights.map(|h| h.spans(line)).unwrap_or(&[]);
    render::runs_for_syntax_line_with_plain(
        &line.text,
        spans,
        &font(theme.font_mono.clone()),
        theme.text.opacity(0.92),
        theme,
    )
}

/// One half of a split row: the same accent bar / gutter / code
/// columns a unified row uses, minus the second gutter — each half numbers
/// only its own side. Takes prebuilt `runs` so a mirrored row can share one
/// set across both columns.
fn split_line_cell(
    line: &DiffLine,
    number: Option<u32>,
    runs: Vec<gpui::TextRun>,
    theme: &Theme,
    gutter_px: f32,
    code_width: DiffCodeWidth,
    scroll: Option<DiffCodeScroll>,
) -> gpui::Div {
    let mut add_bg = add_color(theme);
    add_bg.a = 0.055;
    let mut del_bg = del_color(theme);
    del_bg.a = 0.055;
    let (row_bg, accent, number_color) = line_tones(line.kind, add_bg, del_bg, theme);
    let content_width = match code_width {
        DiffCodeWidth::Clipped => None,
        DiffCodeWidth::Scrollable(metrics) => Some(metrics.split_content_width(gutter_px)),
        DiffCodeWidth::Wrapped => None,
    };
    let wrapped = matches!(code_width, DiffCodeWidth::Wrapped);
    div()
        .flex_1()
        .min_w_0()
        .self_stretch()
        .overflow_hidden()
        .flex()
        .flex_row()
        .items_start()
        .when_some(row_bg, |el, bg| el.bg(bg))
        .child(
            div()
                .w(px(ACCENT_BAR_WIDTH))
                .self_stretch()
                .flex_none()
                .when_some(accent, |el, color| el.bg(color)),
        )
        .child(gutter_cell(
            number,
            number_color,
            wrapped.then_some(line.text.as_str()),
            gutter_px,
            theme,
        ))
        .child(code_text_viewport(
            line.text.clone(),
            runs,
            theme,
            SPLIT_CODE_PADDING_LEFT,
            content_width,
            wrapped,
            scroll,
        ))
}

/// The empty half of a one-sided split row — a pure-insert row has no old
/// line, and vice versa. A flat wash, quieter than either tint, reads as
/// "nothing here" without competing with the code beside it.
fn split_filler() -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .self_stretch()
        .bg(crate::theme::ink(0.03))
}

/// Compose the two halves with the centre hairline.
fn split_row(left: AnyElement, right: AnyElement, wrapped: bool, theme: &Theme) -> gpui::Div {
    div()
        .map(|el| {
            if wrapped {
                el.min_h(px(diff_line_height(theme)))
            } else {
                el.h(px(diff_line_height(theme)))
            }
        })
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_stretch()
        .child(left)
        .child(
            div()
                .w(px(SPLIT_DIVIDER_WIDTH))
                .self_stretch()
                .flex_none()
                .bg(crate::theme::hairline(0.06)),
        )
        .child(right)
}

pub const COMMENT_ADDER_SIZE: f32 = 16.0;

/// A split row's `+` only ever appears in the right column, which carries one
/// gutter — so the offset is the same for every line. It is measured from the
/// column, not the row: the halves are fluid, so the right one has no
/// absolute left edge to measure from.
pub fn split_adder_left(gutter_px: f32) -> f32 {
    ACCENT_BAR_WIDTH + (gutter_px - COMMENT_ADDER_SIZE) / 2.0
}

/// A unified row carries both gutters side by side, and a deletion numbers in
/// the first.
pub fn comment_adder_left(side: CommentSide, gutter_px: f32) -> f32 {
    let column = match side {
        CommentSide::Old => 0.0,
        CommentSide::New => gutter_px,
    };
    ACCENT_BAR_WIDTH + column + (gutter_px - COMMENT_ADDER_SIZE) / 2.0
}

fn positioned_adder(left: f32, adder: AnyElement) -> gpui::Div {
    div()
        .absolute()
        .left(px(left))
        .top(px(0.0))
        .h_full()
        .flex()
        .items_center()
        .child(adder)
}

fn render_comment_adder(
    path: &str,
    side: CommentSide,
    line: u32,
    theme: &Theme,
    cx: &Context<Changes>,
) -> AnyElement {
    let target = path.to_string();
    crate::comment_ui::render_comment_adder(
        format!("cmt-add-{path}-{}-{line}", side.tag()).into(),
        theme,
        cx,
        move |this, window, cx| this.open_draft(target.clone(), side, line, window, cx),
    )
}

/// Mirrors [`ReviewComment::cite_path`] for the not-yet-staged note.
fn draft_cite_path(draft: &CommentDraft) -> &str {
    match draft.side {
        CommentSide::Old => draft.old_path.as_deref().unwrap_or(&draft.path),
        CommentSide::New => &draft.path,
    }
}

/// The expanded body of one file section: notices, unchanged-lines rows, +/-/context
/// lines with a coloured accent bar, dual line-number gutters and
/// paint-only syntax runs (slopty checkout-diff-sidebar).
/// Shared with the transcript's tool-diff detail blocks — the same component
/// renders a checkout diff section and an inline ACP tool diff. (The changes
/// pane itself virtualizes these rows individually; this stacked form serves
/// the transcript and the fold tween's clipped stand-in.)
/// Full-document old/new highlighting for tool and checkout diffs.
pub(crate) fn render_file_body_with_syntax(
    file: &FileDiff,
    highlights: Option<Arc<DiffHighlights>>,
    theme: &Theme,
) -> AnyElement {
    let mut children: Vec<AnyElement> = Vec::new();
    let gutter_px = gutter_width(file);
    for notice in file_notices(file) {
        children.push(notice_row(notice, theme));
    }
    let lead = ACCENT_BAR_WIDTH + 2.0 * gutter_px;
    for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
        let count = unchanged_before(file, hunk_ix);
        if count > 0 {
            children.push(
                unchanged_row(count, &hunk.header, lead, UNIFIED_CODE_PADDING_LEFT, false, theme)
                    .into_any_element(),
            );
        }
        for line in &hunk.lines {
            let spans = highlights
                .as_deref()
                .map(|highlights| highlights.spans(line))
                .unwrap_or(&[]);
            children.push(diff_line_row(
                line,
                spans,
                theme,
                gutter_px,
                DiffCodeWidth::Clipped,
                None,
                &[], // slopty: the find marks only the live rows.
            ));
        }
    }
    div()
        .flex()
        .flex_col()
        .pb(px(BODY_BOTTOM_PAD))
        .children(children)
        .into_any_element()
}

/// Build only rows that start above `max_px` so the fold tween's stand-in
/// never materializes lines its clip cannot reveal.
fn render_file_body_upto(
    file: &FileDiff,
    highlight: Option<Arc<DiffHighlights>>,
    theme: &Theme,
    max_px: f32,
    mode: DiffMode,
    code_width: DiffCodeWidth,
    scroll: Option<DiffCodeScrollContext>,
) -> AnyElement {
    let mut children: Vec<AnyElement> = Vec::new();
    let mut y = 0.0f32;
    let gutter_px = gutter_width(file);
    let wrapped = matches!(code_width, DiffCodeWidth::Wrapped);
    let spans_for = |line: &DiffLine| {
        highlight
            .as_deref()
            .map(|highlights| highlights.spans(line))
            .unwrap_or(&[])
    };

    'build: {
        for notice in file_notices(file) {
            if y >= max_px {
                break 'build;
            }
            children.push(notice_row(notice, theme));
            y += NOTICE_HEIGHT;
        }
        let (lead, pad) = match mode {
            DiffMode::Unified => (ACCENT_BAR_WIDTH + 2.0 * gutter_px, UNIFIED_CODE_PADDING_LEFT),
            DiffMode::Split => (ACCENT_BAR_WIDTH + gutter_px, SPLIT_CODE_PADDING_LEFT),
        };
        for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
            if y >= max_px {
                break 'build;
            }
            let count = unchanged_before(file, hunk_ix);
            if count > 0 {
                children.push(
                    unchanged_row(count, &hunk.header, lead, pad, true, theme).into_any_element(),
                );
                y += UNCHANGED_HEIGHT;
            }
            match mode {
                DiffMode::Unified => {
                    for (line_ix, line) in hunk.lines.iter().enumerate() {
                        if y >= max_px {
                            break 'build;
                        }
                        children.push(diff_line_row(
                            line,
                            spans_for(line),
                            theme,
                            gutter_px,
                            code_width,
                            scroll
                                .as_ref()
                                .map(|scroll| scroll.slot(format_args!("{hunk_ix}-{line_ix}"))),
                            &[], // slopty: the find marks only the live rows.
                        ));
                        y += diff_line_height(theme);
                    }
                }
                DiffMode::Split => {
                    // Pair only what the clip can still reveal: the unified
                    // arm breaks out of a lazy walk, so the split arm must not
                    // materialize the whole hunk first.
                    let budget = ((max_px - y) / diff_line_height(theme)).ceil().max(0.0) as usize;
                    for (pair_ix, (left, right)) in split_pairs_upto(&hunk.lines, budget)
                        .into_iter()
                        .enumerate()
                    {
                        if y >= max_px {
                            break 'build;
                        }
                        let line_at =
                            |slot: Option<u32>| slot.and_then(|slot| hunk.lines.get(slot as usize));
                        let cell = |line: Option<&DiffLine>, old: bool| match line {
                            Some(line) => split_line_cell(
                                line,
                                if old { line.old_no } else { line.new_no },
                                line_runs(line, highlight.as_deref(), theme),
                                theme,
                                gutter_px,
                                code_width,
                                scroll.as_ref().map(|scroll| {
                                    scroll.slot(format_args!(
                                        "{hunk_ix}-{pair_ix}-{}",
                                        if old { "old" } else { "new" }
                                    ))
                                }),
                            )
                            .into_any_element(),
                            None => split_filler().into_any_element(),
                        };
                        let (left, right) = (line_at(left), line_at(right));
                        let marker = [left, right]
                            .into_iter()
                            .flatten()
                            .find(|line| line.kind == LineKind::Meta);
                        children.push(match marker {
                            Some(line) => meta_line_row(
                                &line.text,
                                theme,
                                2.0 * (ACCENT_BAR_WIDTH + gutter_px),
                            ),
                            None => split_row(cell(left, true), cell(right, false), wrapped, theme)
                                .into_any_element(),
                        });
                        y += diff_line_height(theme);
                    }
                }
            }
        }
    }

    div()
        .flex()
        .flex_col()
        .pb(px(BODY_BOTTOM_PAD))
        .children(children)
        .into_any_element()
}

impl Render for Changes {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // slopty: no History surface (ruled out)
        // slopty: the find put away hands the keyboard back to the pane.
        if std::mem::take(&mut self.refocus) {
            window.focus(&self.focus, cx);
        }
        let theme = Theme::of(cx).clone();
        let active = self.active_diff(cx);
        let scope = self.scope;
        let base = self.base_ref.clone();
        // With no session selected (new-chat canvas) there is nothing to
        // prepare — show the quiet empty state, not an endless spinner.
        let no_chat = self.state.read(cx).selected_chat_row().is_none();
        let phase = if no_chat {
            DiffPhase::Clean
        } else {
            diff_phase(active.as_ref())
        };
        let error = self.error.clone();
        // Scoped fetch failures replace the content area. "no turn recorded"
        // is the expected pre-first-turn state, not an error; "unknown
        // method" is version skew — the chat's host engine predates
        // GetCheckoutDiff (a still-running daemon after an app update, or a
        // remote device behind on releases) — say that instead of leaking
        // the raw RPC error (user report).
        // slopty: a notice is a headline and its way on, in the empty-state voice, naming the
        // machine.
        let host = {
            let state = self.state.read(cx);
            state.selected_chat_row().map(|chat| {
                state.device_name(&chat.device_id).unwrap_or(&chat.device_id).to_owned()
            })
        }
        .unwrap_or_else(|| "This chat's machine".to_owned());
        let scoped_notice: Option<(SharedString, Option<SharedString>, bool)> = (!no_chat
            && scope != DiffScope::WorkingTree)
            .then(|| self.scoped_error.clone())
            .flatten()
            .map(|message| {
                if message.contains("no turn recorded") {
                    (
                        "No turns yet".into(),
                        Some("Send a message to see what it changes.".into()),
                        false,
                    )
                } else if message.contains("unknown method") {
                    (
                        format!("{host} runs an older Slopty").into(),
                        Some("Update it to see branch and turn changes.".into()),
                        false,
                    )
                } else {
                    (message, None, true)
                }
            });

        // slopty: the preparing loader waits 150 ms and, once shown, stays 400 ms
        // (`loaders::delayed`), asked on every draw so it knows when the diff lands.
        let preparing = crate::loaders::delayed(
            "changes-preparing",
            scoped_notice.is_none() && phase == DiffPhase::Preparing,
            window,
            cx,
        );
        // slopty: a failed read names what failed and offers the way on (M31).
        let content: AnyElement = if let Some((message, _, true)) = scoped_notice.clone() {
            crate::files::failure_state(
                "changes-scoped-error",
                "Couldn't load this diff",
                message,
                &theme,
                cx.listener(|this, _, _, cx| {
                    this.scoped_for = None;
                    this.scoped_error = None;
                    this.sync(cx);
                    cx.notify();
                }),
                cx,
            )
        } else if let Some((headline, line, _)) = scoped_notice {
            pane_note("changes-notice", headline, line, &theme)
        } else if preparing != crate::loaders::Delayed::Done || phase == DiffPhase::Preparing {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .when(preparing == crate::loaders::Delayed::Shown, |el| {
                    el.child(crate::loaders::pending("changes-preparing", "Preparing diff…").at_once())
                })
                .into_any_element()
        } else {
            match phase {
                DiffPhase::Preparing => gpui::Empty.into_any_element(),
                DiffPhase::Clean => pane_note(
                    "changes-clean",
                    clean_message(scope, base.as_deref()).into(),
                    Some(clean_line(scope).into()),
                    &theme,
                ),
                DiffPhase::List => {
                    // slopty: a diff whose every change was whitespace, read with whitespace
                    // ignored, says so.
                    let only_space = self.ignore_whitespace
                        && self.parsed.as_ref().is_some_and(|parsed| parsed.files.is_empty());
                    if only_space {
                        pane_note(
                            "changes-only-space",
                            "Only whitespace changed".into(),
                            Some("Turn off Ignore whitespace to see it.".into()),
                            &theme,
                        )
                    } else if self.parsed.is_some() {
                        let sticky_header = self.render_sticky_file_header(&theme, cx);
                        // slopty: the changed files' tree beside the diff, and the find over it.
                        let tree = self.render_tree(&theme, cx);
                        let find = self.render_find(&theme, cx);
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .children(self.render_header_strip(&theme))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .flex_row()
                                    .children(tree)
                                    .child(
                                        div()
                                            .relative()
                                            .flex_1()
                                            .min_w_0()
                                            .h_full()
                                            .overflow_hidden()
                                            .child(
                                                list(self.list.clone(), cx.processor(Self::render_row))
                                                    .size_full()
                                                    .with_sizing_behavior(
                                                        gpui::ListSizingBehavior::Auto,
                                                    ),
                                            )
                                            .when_some(sticky_header, |el, header| el.child(header))
                                            .children(find),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        // Diff known, parse still running.
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::loaders::gradient_spinner(
                                "changes-parsing",
                                &theme,
                                3.0,
                                cx.entity_id(),
                                cx,
                            ))
                            .into_any_element()
                    }
                }
            }
        };

        div()
            // slopty: the pane's key context, where the find's actions land.
            .key_context("Changes")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &FindInDiff, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindNextInDiff, _, cx| this.step_find(1, cx)))
            .on_action(cx.listener(|this, _: &FindPreviousInDiff, _, cx| this.step_find(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleDiffTree, _, cx| this.toggle_tree(cx)))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus.contains_focused(window, cx) {
                        window.focus(&this.focus, cx);
                    }
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            // Changes is a code-adjacent surface: chrome stays Geist while
            // paths, hunks, gutters, and source runs keep their mono overrides.
            .font_family(theme.font_sans_fixed.clone())
            .when_some(error, |el, message| {
                el.child(
                    div()
                        .flex_none()
                        .px(px(Theme::SPACE_MD))
                        .py(px(4.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.warning)
                        .child(message),
                )
            })
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // slopty: the review aids' choices outlive the tab and the launch.
    #[gpui::test]
    fn the_review_aids_keep_their_last_choice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let state = cx.new(|_| AppState::new());
        let first = cx.add_window({
            let state = state.clone();
            move |_, cx| Changes::new(state, cx)
        });
        first
            .update(cx, |changes, _, cx| {
                assert!(!changes.ignore_whitespace && !changes.show_tree);
                changes.toggle_ignore_whitespace(cx);
                changes.toggle_tree(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let second = cx.add_window(move |_, cx| Changes::new(state, cx));
        second
            .update(cx, |changes, _, _| {
                assert!(changes.ignore_whitespace && changes.show_tree, "a new tab takes them");
            })
            .unwrap();
        let saved = crate::settings::UiSettings::load(dir.path());
        assert!(saved.diff_ignore_whitespace && saved.diff_show_tree, "and the next launch");
    }

    // slopty: ⇧⌘Y shows and hides the changed files' tree from the pane.
    #[gpui::test]
    fn the_tree_key_toggles_the_changed_files(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            bind_keys(cx);
        });
        let state = cx.new(|_| AppState::new());
        let (changes, cx) = cx.add_window_view(move |_, cx| Changes::new(state, cx));
        changes.update_in(cx, |changes, window, cx| window.focus(&changes.focus, cx));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let focus = changes.read(cx).focus.clone();
            assert_eq!(
                crate::keys::for_action(&ToggleDiffTree, Some(&focus), window, cx),
                Some(crate::keys::combo_text("mod-shift-y"))
            );
        });
        cx.simulate_keystrokes(&crate::settings::platform_combo("mod-shift-y"));
        changes.read_with(cx, |changes, _| assert!(changes.show_tree));
    }

    #[gpui::test]
    fn editing_staged_diff_comments_preserves_identity_and_cancellation(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Changes::new(state, cx)
        });
        window
            .update(cx, |changes, window, cx| {
                for side in [CommentSide::Old, CommentSide::New] {
                    let original =
                        ReviewComment::new("new.rs", side, 7, "Original 🦀\nSecond line")
                            .renamed_from(Some("old.rs"));
                    changes.state.update(cx, |state, _| {
                        state.add_review_comment("", original.clone())
                    });
                    changes.edit_comment(&original.id, window, cx);
                    let draft = changes.draft.as_ref().unwrap();
                    assert_eq!(draft.input.read(cx).text(), original.body);
                    assert_eq!(draft_cite_path(draft), original.cite_path());
                    assert!(draft.input.read(cx).focus_handle(cx).is_focused(window));
                    let input = draft.input.clone();
                    input.update(cx, |input, cx| input.set_text("Cancelled", cx));
                    assert_eq!(changes.state.read(cx).review_comments("")[0], original);
                    changes.cancel_draft(cx);
                    assert_eq!(
                        changes.state.read(cx).review_comments(""),
                        &[original.clone()]
                    );

                    changes.edit_comment(&original.id, window, cx);
                    changes
                        .draft
                        .as_ref()
                        .unwrap()
                        .input
                        .clone()
                        .update(cx, |input, cx| {
                            input.set_text("  Revised\nMore detail  ", cx)
                        });
                    changes.commit_draft(cx);
                    let mut expected = original.clone();
                    expected.body = "Revised\nMore detail".into();
                    assert_eq!(
                        changes.state.read(cx).review_comments(""),
                        &[expected.clone()]
                    );
                    assert_ne!(
                        comment_state_key(&[original], None),
                        comment_state_key(&[expected.clone()], None)
                    );
                    changes.edit_comment(&expected.id, window, cx);
                    let sent = changes
                        .state
                        .update(cx, |state, _| state.take_review_comments(""));
                    assert!(comments::with_comments("", &sent).contains("Revised\n  More detail"));
                    changes.commit_draft(cx);
                    assert!(changes.state.read(cx).review_comments("").is_empty());
                }
            })
            .unwrap();
    }

    const PATCH: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    let x = 1;
 }
@@ -10,2 +11,2 @@
 // tail
-old_line
+new_line
diff --git a/added.txt b/added.txt
new file mode 100644
--- /dev/null
+++ b/added.txt
@@ -0,0 +1,2 @@
+first
+second
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
diff --git a/img.png b/img.png
new file mode 100644
Binary files /dev/null and b/img.png differ
diff --git a/old_name.rs b/new_name.rs
similarity index 90%
rename from old_name.rs
rename to new_name.rs
";

    #[test]
    fn parses_files_hunks_and_lines() {
        let files = parse_patch(PATCH);
        assert_eq!(files.len(), 5);

        let main = &files[0];
        assert_eq!(main.path, "src/main.rs");
        assert_eq!(main.status, FileStatus::Modified);
        assert_eq!(main.hunks.len(), 2);
        assert_eq!(main.additions, 3);
        assert_eq!(main.deletions, 2);
        let h0 = &main.hunks[0];
        assert_eq!(h0.header, "@@ -1,4 +1,5 @@ fn main");
        assert_eq!(h0.lines.len(), 5);
        assert_eq!(h0.lines[0].kind, LineKind::Context);
        assert_eq!(h0.lines[0].old_no, Some(1));
        assert_eq!(h0.lines[0].new_no, Some(1));
        assert_eq!(h0.lines[1].kind, LineKind::Del);
        assert_eq!(h0.lines[1].old_no, Some(2));
        assert_eq!(h0.lines[1].new_no, None);
        assert_eq!(h0.lines[2].kind, LineKind::Add);
        assert_eq!(h0.lines[2].new_no, Some(2));
        assert_eq!(h0.lines[3].kind, LineKind::Add);
        assert_eq!(h0.lines[3].new_no, Some(3));
        // Closing context line: numbering advanced past the add/del block.
        assert_eq!(h0.lines[4].old_no, Some(3));
        assert_eq!(h0.lines[4].new_no, Some(4));
        // Second hunk restarts numbering from its header.
        assert_eq!(main.hunks[1].lines[0].old_no, Some(10));
        assert_eq!(main.hunks[1].lines[0].new_no, Some(11));
    }

    #[test]
    fn detects_new_deleted_binary_and_renamed() {
        let files = parse_patch(PATCH);
        let added = &files[1];
        assert_eq!(added.status, FileStatus::Added);
        assert_eq!(added.additions, 2);
        // The no-newline marker rides as a Meta line.
        let last = added.hunks[0].lines.last().unwrap();
        assert_eq!(last.kind, LineKind::Meta);
        assert!(last.text.contains("No newline"));
        assert!(file_notices(added).iter().any(|n| n == "New file"));

        let deleted = &files[2];
        assert_eq!(deleted.status, FileStatus::Deleted);
        assert_eq!(deleted.deletions, 1);
        assert!(file_notices(deleted).iter().any(|n| n == "Deleted file"));

        let binary = &files[3];
        assert!(binary.binary);
        assert_eq!(binary.status, FileStatus::Added);
        assert!(binary.hunks.is_empty());
        assert!(file_notices(binary).iter().any(|n| n.contains("Binary")));

        let renamed = &files[4];
        assert_eq!(renamed.status, FileStatus::Renamed);
        assert_eq!(renamed.path, "new_name.rs");
        assert_eq!(renamed.old_path.as_deref(), Some("old_name.rs"));
        assert!(
            file_notices(renamed)
                .iter()
                .any(|n| n.contains("old_name.rs"))
        );
    }

    #[test]
    fn empty_and_garbage_patches_parse_to_nothing() {
        assert!(parse_patch("").is_empty());
        assert!(parse_patch("not a diff\nat all\n").is_empty());
        // Truncated mid-hunk: keeps what parsed.
        let files = parse_patch("diff --git a/x b/x\n@@ -1,9 +1,9 @@\n ctx\n+add");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks[0].lines.len(), 2);
        assert_eq!(files[0].additions, 1);
    }

    #[test]
    fn quoted_and_spaced_paths() {
        let (old, new) = parse_git_paths("a/simple.rs b/simple.rs");
        assert_eq!((old.as_str(), new.as_str()), ("simple.rs", "simple.rs"));
        let (old, new) = parse_git_paths("\"a/with space.rs\" \"b/with space.rs\"");
        assert_eq!(old, "with space.rs");
        assert_eq!(new, "with space.rs");
    }

    #[test]
    fn hunk_headers_parse_with_and_without_counts() {
        assert_eq!(parse_hunk_header("@@ -1,4 +2,5 @@"), Some((1, 2)));
        assert_eq!(parse_hunk_header("@@ -7 +9 @@ fn ctx"), Some((7, 9)));
        assert_eq!(parse_hunk_header("@@ garbage"), None);
    }

    #[test]
    fn rows_flatten_to_line_granularity() {
        let files = parse_patch(PATCH);
        let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Unified, |_| false);
        assert_eq!(ranges.len(), files.len());
        // Every file's span starts with its header…
        for (ix, range) in ranges.iter().enumerate() {
            assert_eq!(rows[range.start], DiffRow::FileHeader { file: ix as u32 });
            // …and spans exactly header + analytic body rows.
            assert_eq!(range.len(), 1 + body_row_count(&files[ix]));
        }
        // Spans tile the whole row vec.
        assert_eq!(ranges.last().unwrap().end, rows.len());

        // src/main.rs: header, 8 lines, the 5 unchanged lines between its hunks as one row
        // (none above the first, which opens at line 1), pad.
        let main_rows = &rows[ranges[0].clone()];
        assert_eq!(main_rows.len(), 1 + 1 + 8 + 1);
        assert_eq!(main_rows[6], DiffRow::Unchanged { file: 0, hunk: 1 });
        // Flat line indices run across hunks (they key the highlight slot).
        let flats: Vec<u32> = main_rows
            .iter()
            .filter_map(|r| match r {
                DiffRow::Line { flat, .. } => Some(*flat),
                _ => None,
            })
            .collect();
        assert_eq!(flats, (0..8).collect::<Vec<u32>>());
        assert_eq!(*main_rows.last().unwrap(), DiffRow::BodyPad { file: 0 });

        // A collapsed file contributes its header row only.
        let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Unified, |ix| ix == 0);
        assert_eq!(ranges[0].len(), 1);
        assert_eq!(rows[ranges[1].start], DiffRow::FileHeader { file: 1 });

        // Notices lead the body: the added file carries "New file".
        let added_rows = &rows[ranges[1].clone()];
        assert_eq!(added_rows[1], DiffRow::Notice { file: 1, notice: 0 });
    }

    #[test]
    fn sticky_header_tracks_the_logical_top_row() {
        let ranges = vec![0..4, 4..5, 5..10];

        assert_eq!(sticky_file_header(&[], 0, 0.0), None);
        assert_eq!(sticky_file_header(&ranges, 0, 0.0), None);
        assert_eq!(
            sticky_file_header(&ranges, 0, 0.5),
            Some(StickyFileHeader {
                file_ix: 0,
                header_row: 0,
                next_header_row: Some(4),
            })
        );
        assert_eq!(
            sticky_file_header(&ranges, 2, 0.0),
            Some(StickyFileHeader {
                file_ix: 0,
                header_row: 0,
                next_header_row: Some(4),
            })
        );

        // Landing exactly on a new header hands ownership to that file; its
        // real row remains visible until it starts crossing the viewport.
        assert_eq!(sticky_file_header(&ranges, 4, 0.0), None);
        assert_eq!(
            sticky_file_header(&ranges, 4, 1.0),
            Some(StickyFileHeader {
                file_ix: 1,
                header_row: 4,
                next_header_row: Some(5),
            })
        );
        assert_eq!(sticky_file_header(&ranges, 5, 0.0), None);
        assert_eq!(
            sticky_file_header(&ranges, 8, 0.0),
            Some(StickyFileHeader {
                file_ix: 2,
                header_row: 5,
                next_header_row: None,
            })
        );
        assert_eq!(sticky_file_header(&ranges, 10, 0.0), None);
    }

    #[test]
    fn sticky_header_is_pushed_by_the_next_file() {
        assert_eq!(sticky_header_push_offset(None), 0.0);
        assert_eq!(sticky_header_push_offset(Some(80.0)), 0.0);
        assert_eq!(sticky_header_push_offset(Some(FILE_HEADER_HEIGHT)), 0.0);
        assert_eq!(
            sticky_header_push_offset(Some(FILE_HEADER_HEIGHT - 12.0)),
            -12.0
        );
        assert_eq!(sticky_header_push_offset(Some(0.0)), -FILE_HEADER_HEIGHT);
    }

    #[test]
    fn sticky_header_uses_the_content_theme_in_dark_and_light() {
        use slopty_shell_theme::{AccentSelection, SurfacePreference};

        for (appearance, variant_id) in [
            (crate::theme::Appearance::Dark, "tokyo-night"),
            (crate::theme::Appearance::Light, "rose-pine-dawn"),
        ] {
            let opaque = Theme::for_selection(
                appearance,
                variant_id,
                AccentSelection::ThemeDefault,
                SurfacePreference::Opaque,
            );
            let opaque_paint = sticky_file_header_paint(&opaque);
            assert_eq!(opaque_paint.frost_tint, None, "{variant_id}");
            assert_eq!(
                opaque_paint.rest_bg,
                crate::theme::flatten(opaque.ink(0.025), opaque.bg),
                "{variant_id} opaque background"
            );
            assert_eq!(
                opaque_paint.hover_bg,
                crate::theme::flatten(opaque.element_hover, opaque.bg),
                "{variant_id} opaque hover"
            );
            assert_eq!(opaque_paint.border, opaque.border, "{variant_id} border");

            let frosted = Theme::for_selection(
                appearance,
                variant_id,
                AccentSelection::ThemeDefault,
                SurfacePreference::Frosted,
            );
            let frosted_paint = sticky_file_header_paint(&frosted);
            if frosted.is_frost() {
                let expected_alpha = match appearance {
                    crate::theme::Appearance::Dark => STICKY_FILE_HEADER_TINT_ALPHA_DARK,
                    crate::theme::Appearance::Light => STICKY_FILE_HEADER_TINT_ALPHA_LIGHT,
                };
                let tint = frosted.bg.opacity(expected_alpha);
                assert_eq!(
                    frosted_paint.frost_tint,
                    Some(tint),
                    "{variant_id} content-plane tint"
                );
                assert_ne!(
                    tint,
                    frosted.glass_overlay(),
                    "{variant_id} must not borrow the elevated overlay plane"
                );
                assert_eq!(tint.a, expected_alpha, "{variant_id} tint coverage");
                assert_eq!(
                    frosted_paint.hover_bg,
                    frosted.glass_hover(),
                    "{variant_id} themed hover"
                );
            } else {
                assert_eq!(frosted_paint.frost_tint, None, "{variant_id}");
            }
            assert_eq!(
                frosted_paint.border, frosted.border,
                "{variant_id} frosted border"
            );
        }
    }

    #[test]
    fn split_pairs_align_edits_and_strand_the_rest() {
        let files = parse_patch(PATCH);
        // src/main.rs hunk 0: context, −1, +1, +1, context. The edited line
        // pairs across; the extra addition is stranded on the right.
        assert_eq!(
            split_pairs(&files[0].hunks[0].lines),
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(2)),
                (None, Some(3)),
                (Some(4), Some(4)),
            ]
        );
        // A pure add: every row is right-only, including the trailing
        // no-newline Meta line — it belongs to the side it follows, and its
        // row spans both columns at render.
        assert_eq!(
            split_pairs(&files[1].hunks[0].lines),
            vec![(None, Some(0)), (None, Some(1)), (None, Some(2))]
        );
        // A pure delete strands the left.
        assert_eq!(split_pairs(&files[2].hunks[0].lines), vec![(Some(0), None)]);
        assert!(split_pairs(&[]).is_empty());

        // `-a +b -c +d` is two one-line edits, not one four-line one: a
        // deletion arriving after additions opens a new block.
        let line = |kind| DiffLine {
            kind,
            old_no: Some(1),
            new_no: Some(1),
            text: String::new(),
        };
        let lines = [
            line(LineKind::Del),
            line(LineKind::Add),
            line(LineKind::Del),
            line(LineKind::Add),
        ];
        assert_eq!(
            split_pairs(&lines),
            vec![(Some(0), Some(1)), (Some(2), Some(3))]
        );
    }

    #[test]
    fn no_newline_markers_keep_their_edit_paired() {
        // Both files lost their final newline: git writes the marker twice,
        // once per side. Neither may split the edit into one-sided rows.
        let both = "diff --git a/a.txt b/a.txt\n\
             --- a/a.txt\n\
             +++ b/a.txt\n\
             @@ -1 +1 @@\n\
             -old\n\
             \\ No newline at end of file\n\
             +new\n\
             \\ No newline at end of file\n";
        let files = parse_patch(both);
        let lines = &files[0].hunks[0].lines;
        assert_eq!(
            lines.iter().map(|line| line.kind).collect::<Vec<_>>(),
            vec![LineKind::Del, LineKind::Meta, LineKind::Add, LineKind::Meta]
        );
        // One aligned old/new row, then the two markers on one row of their
        // own — four lines read as two rows, not four.
        assert_eq!(
            split_pairs(lines),
            vec![(Some(0), Some(2)), (Some(1), Some(3))]
        );
        let full = split_pairs(lines);
        for cap in 0..=full.len() + 2 {
            assert_eq!(split_pairs_upto(lines, cap), full[..cap.min(full.len())]);
        }

        // Only the old file lacked one: the edit still pairs, and the lone
        // marker takes a row on its own side.
        let old_only = "diff --git a/a.txt b/a.txt\n\
             --- a/a.txt\n\
             +++ b/a.txt\n\
             @@ -1 +1 @@\n\
             -old\n\
             \\ No newline at end of file\n\
             +new\n";
        let files = parse_patch(old_only);
        assert_eq!(
            split_pairs(&files[0].hunks[0].lines),
            vec![(Some(0), Some(2)), (Some(1), None)]
        );
    }

    #[test]
    fn split_flattening_pairs_rows_and_keeps_heights_analytic() {
        let files = parse_patch(PATCH);
        let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Split, |_| false);
        assert_eq!(ranges.len(), files.len());
        assert_eq!(ranges.last().unwrap().end, rows.len());

        // src/main.rs: header, the unchanged lines between its hunks (none before the first),
        // 4 + 2 paired rows, pad — the same 8 lines, two columns.
        let main_rows = &rows[ranges[0].clone()];
        assert_eq!(main_rows.len(), 1 + 1 + (4 + 2) + 1);
        assert_eq!(
            main_rows[1],
            DiffRow::SplitLine {
                file: 0,
                hunk: 0,
                left: Some(0),
                right: Some(0),
            }
        );
        assert_eq!(
            main_rows[3],
            DiffRow::SplitLine {
                file: 0,
                hunk: 0,
                left: None,
                right: Some(3),
            }
        );
        assert_eq!(main_rows[5], DiffRow::Unchanged { file: 0, hunk: 1 });
        assert_eq!(*main_rows.last().unwrap(), DiffRow::BodyPad { file: 0 });
        // Pairing only ever merges rows, so split is never the taller layout.
        assert!(main_rows.len() < 1 + body_row_count(&files[0]));

        // Heights stay analytic — the fold tween needs no measurement.
        assert_eq!(
            body_height_with(&files[0], &[], None, DiffMode::Split, DIFF_LINE_HEIGHT),
            UNCHANGED_HEIGHT + 6.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
        );
    }

    #[test]
    fn capped_pairing_agrees_with_the_full_pairing_and_stays_bounded() {
        // The fold tween re-renders its stand-in every frame, so the capped
        // walk must be a true prefix of the full one — not an approximation.
        let lines = &parse_patch(PATCH)[0].hunks[0].lines;
        let full = split_pairs(lines);
        for cap in 0..=full.len() + 2 {
            assert_eq!(split_pairs_upto(lines, cap), full[..cap.min(full.len())]);
        }

        // A huge single-sided run must not be materialized to yield a few
        // rows: 20k deletions, 5 rows asked for, 5 rows built.
        let many: Vec<DiffLine> = (0..20_000u32)
            .map(|n| DiffLine {
                kind: LineKind::Del,
                old_no: Some(n + 1),
                new_no: None,
                text: String::new(),
            })
            .collect();
        let capped = split_pairs_upto(&many, 5);
        assert_eq!(capped.len(), 5);
        assert!(
            capped.capacity() < 100,
            "capacity tracks the cap, not the hunk"
        );
        assert_eq!(capped[4], (Some(4), None));
    }

    #[test]
    fn a_split_row_offers_each_column_its_own_anchor() {
        let files = parse_patch(PATCH);
        let lines = &files[0].hunks[0].lines;
        // The paired edit cites the old line on the left, the new on the right.
        assert_eq!(
            pair_anchors(lines, (Some(1), Some(2))),
            [Some((CommentSide::Old, 2)), Some((CommentSide::New, 2))]
        );
        // A context row names one anchor, not the same one twice — otherwise
        // its card would be pushed into the body in duplicate. The caller
        // flattens, so the dropped duplicate reads as an empty slot.
        assert_eq!(
            pair_anchors(lines, (Some(0), Some(0))),
            [Some((CommentSide::New, 1)), None]
        );
        // A stranded side contributes nothing.
        assert_eq!(
            pair_anchors(lines, (None, Some(3))),
            [None, Some((CommentSide::New, 3))]
        );
    }

    #[test]
    fn split_rows_carry_the_comments_of_both_columns() {
        let files = parse_patch(PATCH);
        // A context row must not stack the same card twice.
        let comment = ReviewComment::new("src/main.rs", CommentSide::New, 1, "why");
        let rows = body_rows(0, &files[0], &[comment], None, DiffMode::Split);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, DiffRow::CommentCard { .. }))
                .count(),
            1
        );

        // Both sides of one paired row hang off that row, in column order.
        let staged = vec![
            ReviewComment::new("src/main.rs", CommentSide::Old, 2, "left"),
            ReviewComment::new("src/main.rs", CommentSide::New, 2, "right"),
        ];
        let rows = body_rows(0, &files[0], &staged, None, DiffMode::Split);
        let edit = rows
            .iter()
            .position(|row| matches!(row, DiffRow::SplitLine { left: Some(1), .. }))
            .unwrap();
        assert_eq!(rows[edit + 1], DiffRow::CommentCard { file: 0, card: 0 });
        assert_eq!(rows[edit + 2], DiffRow::CommentCard { file: 0, card: 1 });
    }

    #[test]
    fn a_split_rows_right_column_is_never_a_deletion() {
        // The invariant the `+` placement rests on: only the right column is
        // hoverable, so every note a split row can start must cite the
        // post-change file. Were a deletion ever to land on the right, that
        // rule would quietly start filing notes against lines the agent
        // cannot edit.
        for file in parse_patch(PATCH) {
            for hunk in &file.hunks {
                for (_, right) in split_pairs(&hunk.lines) {
                    let Some(line) = right.and_then(|ix| hunk.lines.get(ix as usize)) else {
                        continue;
                    };
                    assert_ne!(line.kind, LineKind::Del, "{:?}", line);
                    assert!(matches!(
                        line_anchor(line),
                        None | Some((CommentSide::New, _))
                    ));
                }
            }
        }
    }

    // slopty: a large diff opens with a file past the file budget collapsed, and every file that
    // would take the open ones past the diff budget; each header then loads its own body alone.
    #[test]
    fn a_large_diff_opens_with_files_past_the_budget_collapsed() {
        let patch_of = |name: &str, lines: usize| {
            format!("diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -0,0 +1,{lines} @@\n")
                + &"+x\n".repeat(lines)
        };
        let sizes = [10, LARGE_FILE_LINES + 1, 1_400, 1_400, 1_400, 900];
        let patch: String = sizes
            .iter()
            .enumerate()
            .map(|(ix, lines)| patch_of(&format!("f{ix}.rs"), *lines))
            .collect();
        let files = parse_patch(&patch);
        assert_eq!(files.iter().map(diff_lines).collect::<Vec<_>>(), sizes);
        // Spent: 10; the large one held; 1 410; 2 810; 4 210 would pass the budget; 3 710 fits.
        let held = held_back(&files);
        assert_eq!(held, [false, true, false, false, true, false]);
        let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Unified, |ix| held[ix]);
        for (ix, range) in ranges.iter().enumerate() {
            assert_eq!(range.len() == 1, held[ix], "f{ix}: a held file is its header alone");
        }
        let open: usize = files.iter().zip(&held).filter(|(_, h)| !**h).map(|(f, _)| diff_lines(f)).sum();
        assert!(open <= DIFF_LINE_BUDGET, "{open} lines open");
        assert!(rows.len() < open + 3 * files.len(), "{} rows", rows.len());
        // A diff within both budgets holds nothing back.
        assert_eq!(held_back(&parse_patch(&patch_of("a.rs", LARGE_FILE_LINES))), [false]);
    }

    #[test]
    fn truncate_caps_lines_and_appends_notice() {
        let mut file = parse_patch(PATCH).remove(0); // 2 hunks, 8 lines
        let untouched = file.clone();
        truncate_file_lines(&mut file, 10);
        assert_eq!(file, untouched, "under the cap: untouched");

        truncate_file_lines(&mut file, 6);
        let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
        assert_eq!(lines, 6);
        assert_eq!(file.hunks.len(), 2);
        assert!(
            file_notices(&file)
                .iter()
                .any(|n| n.contains("first 6 of 8 lines"))
        );
        // body_height stays consistent with what actually renders.
        assert_eq!(
            body_height(&file),
            NOTICE_HEIGHT + UNCHANGED_HEIGHT + 6.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
        );

        // A cap below the first hunk's length drops later hunks entirely.
        let mut file = parse_patch(PATCH).remove(0);
        truncate_file_lines(&mut file, 3);
        assert_eq!(file.hunks.len(), 1);
        assert_eq!(file.hunks[0].lines.len(), 3);
    }

    #[test]
    fn gutters_fit_the_largest_line_number() {
        let files = parse_patch(PATCH);
        // src/main.rs second hunk ends at old 11 / new 12.
        assert_eq!(files[0].max_line, 12);
        assert_eq!(gutter_width(&files[0]), GUTTER_WIDTH);

        // Every digit count keeps ≥6px clear of the accent bar on the left
        // of the number (digits×6.6 + 8px right pad + 6px gap), and the
        // column never shrinks below its floor.
        let mut file = files[0].clone();
        for digits in 1..=7u32 {
            file.max_line = 10u32.pow(digits) - 1;
            let w = gutter_width(&file);
            assert!(w >= GUTTER_WIDTH);
            let left_gap = w - (digits as f32 * 6.6 + 8.0);
            assert!(
                left_gap >= 6.0,
                "{digits} digits: left gap {left_gap} < 6px"
            );
        }
        // slopty: the floor holds three digits, so a file under a thousand lines keeps one
        // width as it grows, and the fourth digit is the first to widen it (item 11).
        file.max_line = 9;
        assert_eq!(gutter_width(&file), GUTTER_WIDTH);
        file.max_line = 999;
        assert_eq!(gutter_width(&file), GUTTER_WIDTH);
        assert!(gutter_width(&file) < 36.0, "the old 36 px column at three digits");
        file.max_line = 9999;
        assert!(gutter_width(&file) > GUTTER_WIDTH);
        file.max_line = 27404;
        assert!(
            gutter_width(&file)
                > gutter_width(&{
                    let mut f = file.clone();
                    f.max_line = 9999;
                    f
                })
        );

        // Truncation refits the gutter to what actually renders: the first
        // 3 lines are ctx(1,1) / del(2,·) / add(·,2) — max line 2.
        let mut file = files[0].clone();
        truncate_file_lines(&mut file, 3);
        assert_eq!(file.max_line, 2);
    }

    #[test]
    fn horizontal_geometry_counts_tabs_and_unicode_columns() {
        assert_eq!(visual_columns("ab\tc"), 5);
        assert_eq!(visual_columns("界"), 2);
        assert_eq!(visual_columns("e\u{301}"), 1);

        let files = parse_patch("diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+ab\t界\n");
        let geometry = DiffHorizontalGeometry::from_file(&files[0]);
        assert_eq!(geometry.max_code_columns, 6);
        assert_eq!(geometry.max_gutter_width, GUTTER_WIDTH);
    }

    #[test]
    fn horizontal_content_width_compensates_for_local_gutters() {
        let metrics = DiffHorizontalMetrics {
            max_text_width: 240.0,
            max_gutter_width: 52.0,
        };
        let narrow = 36.0;
        let wide = 52.0;

        let unified_total = |gutter| {
            ACCENT_BAR_WIDTH + 2.0 * gutter + metrics.unified_content_width(gutter)
        };
        assert_eq!(unified_total(narrow), unified_total(wide));

        let split_total = |gutter| {
            ACCENT_BAR_WIDTH + gutter + metrics.split_content_width(gutter)
        };
        assert_eq!(split_total(narrow), split_total(wide));
    }

    /// Uses the native font backend, not TestAppContext's simulated metrics.
    #[test]
    #[ignore = "slopty: gpui-fast makes its Mac platform on the main thread only, and a test runs on another"]
    fn native_diff_font_geometry() {
        // This regression needs actual DirectWrite/CoreText/fontconfig metrics.
        let (_native, text_system) = crate::typography::native_text_system();
        let mut theme = Theme::dark();
        theme.font_mono = "Geist".into();
        let wide = "W".repeat(100);
        let source = format!("{wide}\n{}\n\t漢字🙂e\u{301}\n", "WWW(WWW);".repeat(200));
        let patch = format!(
            "diff --git a/x.ts b/x.ts\n@@ -0,0 +1,3 @@\n+{}",
            source.trim_end_matches('\n').replace('\n', "\n+")
        );
        let files = parse_patch(&patch);
        let file = &files[0];
        let state = FileHorizontalState::new(file);
        let highlight = Arc::new(DiffHighlights {
            old: None,
            new: Some(Arc::new(
                slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                    source: &source,
                    path: Some("x.ts"),
                    fence_tag: None,
                })
                .unwrap(),
            )),
        });
        let mono = font(theme.font_mono.clone());
        let column = text_system
            .ch_advance(text_system.resolve_font(&mono), px(12.))
            .unwrap()
            .as_f32();
        let line = &file.hunks[0].lines[0];
        let width = text_system
            .shape_line(wide.into(), px(12.), &line_runs(line, None, &theme), None)
            .width()
            .as_f32();
        assert!(
            width > 100. * column * 1.1,
            "requires a real proportional font: {width} vs {}",
            100. * column
        );

        // Simulate plain -> excerpt -> full highlighting, including replacement
        // while an old cached Arc is still alive. Every paint must be reachable.
        for size in [12.5, 32., 8.] {
            theme.code_font_size = size;
            for highlights in [
                None,
                Some(highlight.clone()),
                Some(Arc::new(DiffHighlights {
                    old: None,
                    new: highlight.new.clone(),
                })),
            ] {
                let metrics = state.metrics(file, highlights.as_ref(), &theme, &text_system, 0);
                for line in &file.hunks[0].lines {
                    let runs = line_runs(line, highlights.as_deref(), &theme);
                    let painted = text_system
                        .shape_line(
                            line.text.clone().into(),
                            px(diff_text_size(&theme)),
                            &runs,
                            None,
                        )
                        .width()
                        .as_f32();
                    assert!(metrics.max_text_width >= painted);
                    assert!(
                        metrics.unified_content_width(gutter_width(file))
                            >= UNIFIED_CODE_PADDING_LEFT + painted
                    );
                    assert!(
                        metrics.split_content_width(gutter_width(file))
                            >= SPLIT_CODE_PADDING_LEFT + painted
                    );
                }
                assert_eq!(
                    state.metrics(file, highlights.as_ref(), &theme, &text_system, 0),
                    metrics
                );
            }
        }
    }

    #[test]
    fn horizontal_scroll_and_width_are_independent_per_file() {
        let files = parse_patch(
            "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+short\n\
             diff --git a/b b/b\n@@ -1 +1 @@\n-old\n+a much longer source line\n",
        );
        let states: Vec<_> = files.iter().map(FileHorizontalState::new).collect();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].geometry.max_code_columns, 5);
        assert_eq!(states[1].geometry.max_code_columns, 25);

        let row = DiffRow::Line {
            file: 0,
            hunk: 0,
            line: 0,
            flat: 0,
        };
        let folding = DiffRow::FoldingBody { file: 0 };
        let split = DiffRow::SplitLine {
            file: 1,
            hunk: 0,
            left: Some(0),
            right: Some(1),
        };
        let first = DiffCodeScrollContext {
            handle: states[row.file()].scroll.clone(),
            prefix: "first".into(),
        };
        let second = DiffCodeScrollContext {
            handle: states[split.file()].scroll.clone(),
            prefix: "second".into(),
        };
        first
            .slot("unified")
            .handle
            .set_offset(gpui::Point::new(px(-96.0), px(0.0)));
        assert_eq!(states[folding.file()].scroll.offset().x, px(-96.0));
        assert_eq!(second.slot("old").handle.offset(), gpui::Point::default());

        second
            .slot("old")
            .handle
            .set_offset(gpui::Point::new(px(-48.0), px(0.0)));
        assert_eq!(second.slot("new").handle.offset().x, px(-48.0));
        assert_eq!(first.slot("unified").handle.offset().x, px(-96.0));
    }

    #[test]
    fn horizontal_scroll_reset_returns_to_origin() {
        let handle = gpui::ScrollHandle::new();
        handle.set_offset(gpui::Point::new(px(-120.0), px(-18.0)));

        reset_horizontal_scroll(&handle);

        assert_eq!(handle.offset(), gpui::Point::default());
    }

    #[test]
    fn horizontal_scroll_slots_share_offset_but_keep_unique_ids() {
        let context = DiffCodeScrollContext {
            handle: gpui::ScrollHandle::new(),
            prefix: "row-7".into(),
        };
        let old = context.slot("old");
        let new = context.slot("new");

        old.handle.set_offset(gpui::Point::new(px(-96.0), px(0.0)));

        assert_eq!(new.handle.offset(), old.handle.offset());
        assert_ne!(old.id, new.id);
    }

    #[test]
    fn body_height_is_analytic() {
        let files = parse_patch(PATCH);
        let main = &files[0];
        let lines: usize = main.hunks.iter().map(|h| h.lines.len()).sum();
        assert_eq!(
            body_height(main),
            UNCHANGED_HEIGHT + lines as f32 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
        );
        // Notices add height (added file: 1 notice + meta line inside hunk); its one hunk
        // has no `@@` row, the notice saying it.
        let added = &files[1];
        assert_eq!(
            body_height(added),
            NOTICE_HEIGHT + 3.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
        );
    }

    // slopty: a wrapped line marks each row it wraps onto with `»` under its number, as many as
    // the narrowest pane could wrap it onto; a short line has none.
    #[test]
    fn a_wrapped_line_marks_its_continuation_rows() {
        assert_eq!(continuation_marks("short"), None);
        assert_eq!(continuation_marks(&"x".repeat(50)).as_deref(), Some("»\n»"));
        assert_eq!(continuation_marks(&"\t".repeat(6)).as_deref(), Some("»"), "a tab is four columns");
        assert_eq!(continuation_marks(&"字".repeat(12)).as_deref(), Some("»"), "a wide glyph is two");
        let marks = continuation_marks(&"y".repeat(100_000)).unwrap();
        assert_eq!(marks.matches('»').count(), 200, "bounded");
    }

    // slopty: five squares say how much a file changed, in each side's share.
    #[test]
    fn the_diffstat_fills_a_square_per_line_up_to_five_by_share() {
        use CountKind::{Added as A, Deleted as D, None as N};
        assert_eq!(diffstat_squares(0, 0), [N; 5]);
        assert_eq!(diffstat_squares(2, 0), [A, A, N, N, N], "a two-line change fills two");
        assert_eq!(diffstat_squares(19, 4), [A, A, A, A, D]);
        assert_eq!(diffstat_squares(400, 1), [A, A, A, A, D], "a side that changed keeps one");
        assert_eq!(diffstat_squares(1, 400), [A, D, D, D, D]);
        assert_eq!(diffstat_squares(0, 9), [D; 5]);
    }

    // slopty: between hunks the diff counts what it left out, and a click shows those lines from
    // the file's new side, the nearest first, until none are left (item 11).
    #[test]
    fn unchanged_lines_count_between_hunks_and_open_from_the_new_side() {
        assert_eq!(hunk_span("@@ -3,6 +3,7 @@ fn main() {"), Some([(3, 6), (3, 7)]));
        assert_eq!(hunk_span("@@ -0,0 +1 @@"), Some([(1, 0), (1, 1)]));
        assert_eq!(hunk_span("@@ -5,2 +4,0 @@"), Some([(5, 2), (5, 0)]));
        assert_eq!(hunk_span("garbage"), None);
        assert_eq!(split_hunk_header("@@ -3,6 +3,7 @@ fn main() {").1, "fn main() {");

        let files = parse_patch(PATCH);
        assert_eq!(unchanged_before(&files[0], 0), 0, "the first hunk opens at line 1");
        assert_eq!(unchanged_before(&files[0], 1), 5, "lines 6 to 10 sit between the hunks");
        assert_eq!(unchanged_before(&files[1], 0), 0, "a new file is all change");
        assert_eq!(unchanged_before(&files[2], 0), 0, "and so is a deleted one");

        // The new side, line for line: the first hunk's four lines, line 5 (the patch's header
        // counts five), the five left out, then the second hunk's.
        let mut source: Vec<String> =
            ["fn main() {", "    println!(\"new\");", "    let x = 1;", "}"].map(String::from).into();
        source.extend((5..=10).map(|n| format!("line {n}")));
        source.extend(["// tail", "new_line"].map(String::from));

        let mut file = files[0].clone();
        assert!(reveal_unchanged(&mut file, 1, 2, &source));
        assert_eq!(unchanged_before(&file, 1), 3, "two of five shown");
        let shown = &file.hunks[1].lines[..2];
        assert!(shown.iter().all(|line| line.kind == LineKind::Context));
        assert_eq!(shown[0].text, "line 9");
        assert_eq!((shown[0].old_no, shown[0].new_no), (Some(8), Some(9)));
        assert_eq!(file.hunks[1].header, "@@ -8,4 +9,4 @@");

        assert!(reveal_unchanged(&mut file, 1, u32::MAX, &source));
        assert_eq!(unchanged_before(&file, 1), 0);
        assert!(!reveal_unchanged(&mut file, 1, u32::MAX, &source), "nothing left to show");
        assert!(
            !body_rows(0, &file, &[], None, DiffMode::Unified)
                .iter()
                .any(|row| matches!(row, DiffRow::Unchanged { .. })),
            "a gap shown whole leaves no row"
        );
        assert!(!reveal_unchanged(&mut files[0].clone(), 1, 5, &source[..8]), "a short source shows nothing");
    }

    fn diff(checkout: &str, device: &str, cwd: &str, patch: &str) -> CheckoutDiff {
        CheckoutDiff {
            checkout_id: checkout.into(),
            device_id: device.into(),
            cwd: cwd.into(),
            patch: patch.into(),
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            truncated: false,
            checksum: format!("sum-{}", patch.len()),
            updated_at: Utc::now(),
        }
    }

    fn chat(checkout: Option<&str>, device: &str, cwd: Option<&str>) -> Chat {
        Chat {
            id: "c1".into(),
            device_id: device.into(),
            title: None,
            archived: false,
            cwd: cwd.map(Into::into),
            branch: None,
            checkout_id: checkout.map(Into::into),
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            parent_chat_id: None,
            subagent: false, // slopty: subagents
            in_parent: false,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            draft: None, // slopty: new field.
        }
    }

    #[test]
    fn diff_resolution_prefers_checkout_id_then_cwd() {
        let diffs = vec![
            diff("co-1", "dev-a", "/repo/one", "x"),
            diff("co-2", "dev-b", "/repo/two", "y"),
        ];
        // checkout_id match wins even when cwd points elsewhere.
        let c = chat(Some("co-2"), "dev-a", Some("/repo/one"));
        assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-2");
        // Unknown checkout falls back to device+cwd.
        let c = chat(Some("co-9"), "dev-a", Some("/repo/one"));
        assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-1");
        // Wrong device still matches by cwd alone.
        let c = chat(None, "dev-z", Some("/repo/two"));
        assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-2");
        // Nothing to go on.
        let c = chat(None, "dev-a", None);
        assert!(resolve_diff(&diffs, &c).is_none());
        let c = chat(None, "dev-a", Some("/elsewhere"));
        assert!(resolve_diff(&diffs, &c).is_none());
    }

    #[test]
    fn phases() {
        assert_eq!(diff_phase(None), DiffPhase::Preparing);
        let clean = diff("co", "d", "/w", "  \n");
        assert_eq!(diff_phase(Some(&clean)), DiffPhase::Clean);
        let full = diff("co", "d", "/w", "diff --git a/x b/x\n");
        assert_eq!(diff_phase(Some(&full)), DiffPhase::List);
        // Engine may report files without patch text (truncation edge).
        let mut summarized = diff("co", "d", "/w", "");
        summarized.files.push(slopty_view_proto::DiffFileSummary {
            path: "x".into(),
            old_path: None,
            status: "modified".into(),
            additions: 1,
            deletions: 0,
            binary: false,
        });
        assert_eq!(diff_phase(Some(&summarized)), DiffPhase::List);
    }

    // slopty: a header shows only the counts that say something.
    #[test]
    fn a_file_header_counts_only_what_changed() {
        use CountKind::{Added, Deleted, None};
        let counts = |adds, dels, binary| file_counts(adds, dels, binary);
        assert_eq!(counts(1, 0, false), [("+1".to_owned(), Added)], "no red −0");
        assert_eq!(counts(0, 3, false), [("−3".to_owned(), Deleted)]);
        assert_eq!(counts(2, 3, false), [("+2".to_owned(), Added), ("−3".to_owned(), Deleted)]);
        assert_eq!(counts(0, 0, false), [("0".to_owned(), None)], "a mode-only change");
        assert!(counts(0, 0, true).is_empty(), "a binary says BIN instead");
    }

    #[test]
    fn header_label_pluralizes() {
        assert_eq!(uncommitted_label(0), "0 uncommitted changes");
        assert_eq!(uncommitted_label(1), "1 uncommitted change");
        assert_eq!(uncommitted_label(4), "4 uncommitted changes");
    }

    #[test]
    fn scope_labels_and_clean_messages() {
        assert_eq!(
            scope_label(DiffScope::WorkingTree, 2, None),
            "2 uncommitted changes"
        );
        assert_eq!(
            scope_label(DiffScope::Branch, 1, Some("main")),
            "1 changed file vs main"
        );
        assert_eq!(scope_label(DiffScope::Branch, 3, None), "3 changed files");
        assert_eq!(
            scope_label(DiffScope::LatestTurn, 2, None),
            "2 changed files this turn"
        );
        assert_eq!(
            clean_message(DiffScope::WorkingTree, None),
            "No uncommitted changes"
        );
        assert_eq!(
            clean_message(DiffScope::Branch, Some("develop")),
            "No changes vs develop"
        );
        assert_eq!(
            clean_message(DiffScope::LatestTurn, None),
            "No changes this turn"
        );
    }

    #[test]
    fn base_ref_defaults_to_repo_default_then_main() {
        let branches =
            |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
        // Engine order puts the repo default first — take it when it isn't
        // the checked-out branch itself.
        let b = branches(&["main", "feature"]);
        assert_eq!(
            default_base_ref(&b, Some("feature")).as_deref(),
            Some("main")
        );
        // No origin/HEAD: engine "default" is the current branch — fall
        // through to main/master.
        let b = branches(&["feature", "main"]);
        assert_eq!(
            default_base_ref(&b, Some("feature")).as_deref(),
            Some("main")
        );
        let b = branches(&["feature", "master"]);
        assert_eq!(
            default_base_ref(&b, Some("feature")).as_deref(),
            Some("master")
        );
        // No main/master: any branch that isn't the current one.
        let b = branches(&["feature", "develop"]);
        assert_eq!(
            default_base_ref(&b, Some("feature")).as_deref(),
            Some("develop")
        );
        // Checked out ON main: comparing main with itself is the honest
        // default (empty branch diff).
        let b = branches(&["main", "feature"]);
        assert_eq!(default_base_ref(&b, Some("main")).as_deref(), Some("main"));
        // Single-branch repo, and empty list.
        let b = branches(&["main"]);
        assert_eq!(default_base_ref(&b, Some("main")).as_deref(), Some("main"));
        assert_eq!(default_base_ref(&[], Some("main")), None);
    }

    #[test]
    fn scope_modes_are_wire_stable() {
        // `mode` is the GetCheckoutDiff wire contract — engine matches on it.
        assert_eq!(DiffScope::WorkingTree.mode(), "workingTree");
        assert_eq!(DiffScope::Branch.mode(), "branch");
        assert_eq!(DiffScope::LatestTurn.mode(), "turn");
        assert_eq!(DiffScope::Turn.mode(), "turn", "slopty: any turn's, named by `turn`");
        assert_eq!(DiffScope::default(), DiffScope::WorkingTree);
    }

    // slopty: an earlier turn reads as one.
    #[test]
    fn an_earlier_turn_has_its_own_words() {
        assert_eq!(DiffScope::Turn.label(), "Turn");
        assert_eq!(scope_label(DiffScope::Turn, 1, None), "1 changed file in this turn");
        assert_eq!(clean_message(DiffScope::Turn, None), "No changes in this turn");
    }

    #[test]
    // slopty: no History surface (ruled out)
    fn diff_scope_menu_offers_three_scopes() {
        assert_eq!(
            DiffScope::ALL,
            [
                DiffScope::WorkingTree,
                DiffScope::Branch,
                DiffScope::LatestTurn,
            ]
        );
    }

    #[test]
    fn diff_frames_replace_lists_and_upsert_singles() {
        let mut diffs = Vec::new();
        let one = diff("co-1", "d", "/w", "p1");
        // Single frame inserts.
        assert!(apply_diff_frame(
            &mut diffs,
            serde_json::to_value(&one).unwrap()
        ));
        assert_eq!(diffs.len(), 1);
        // Identical frame is a no-op.
        assert!(!apply_diff_frame(
            &mut diffs,
            serde_json::to_value(&one).unwrap()
        ));
        // Same checkout upserts in place.
        let mut updated = one.clone();
        updated.patch = "p2".into();
        assert!(apply_diff_frame(
            &mut diffs,
            serde_json::to_value(&updated).unwrap()
        ));
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].patch, "p2");
        // List frame replaces wholesale.
        let two = diff("co-2", "d", "/x", "q");
        assert!(apply_diff_frame(
            &mut diffs,
            serde_json::to_value(vec![two.clone()]).unwrap()
        ));
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].checkout_id, "co-2");
        // Malformed frames change nothing.
        assert!(!apply_diff_frame(
            &mut diffs,
            serde_json::json!({"nope": true})
        ));
        assert_eq!(diffs[0].checkout_id, "co-2");
    }

    #[test]
    fn full_diff_highlights_map_old_new_and_context_by_source_line() {
        let old_source = "export function old(value: string) {\n    return value.trim();\n}\n";
        let new_source = "export function new(value: string) {\n    return value.trim();\n}\n";
        let parse = |source| {
            Arc::new(
                slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                    source,
                    path: Some("src/derive.ts"),
                    fence_tag: None,
                })
                .unwrap(),
            )
        };
        let highlights = DiffHighlights {
            old: Some(parse(old_source)),
            new: Some(parse(new_source)),
        };
        let deleted = DiffLine {
            kind: LineKind::Del,
            old_no: Some(1),
            new_no: None,
            text: "export function old(value: string) {".into(),
        };
        let added = DiffLine {
            kind: LineKind::Add,
            old_no: None,
            new_no: Some(1),
            text: "export function new(value: string) {".into(),
        };
        let context = DiffLine {
            kind: LineKind::Context,
            old_no: Some(2),
            new_no: Some(2),
            text: "    return value.trim();".into(),
        };
        assert_eq!(
            highlights.source_ref(&deleted),
            Some(SourceLineRef {
                side: SourceSide::Old,
                line_number: 1
            })
        );
        assert_eq!(
            highlights.source_ref(&added),
            Some(SourceLineRef {
                side: SourceSide::New,
                line_number: 1
            })
        );
        assert_eq!(
            highlights.source_ref(&context),
            Some(SourceLineRef {
                side: SourceSide::New,
                line_number: 2
            })
        );
        assert!(
            highlights
                .spans(&deleted)
                .iter()
                .any(|span| span.kind == slopty_syntax::HighlightKind::Function)
        );
        assert!(
            highlights
                .spans(&added)
                .iter()
                .any(|span| span.kind == slopty_syntax::HighlightKind::Function)
        );
    }

    /// The regression this guards: rendering the diff at the raw shared
    /// setting silently enlarged it from 12.0 to 12.5 on a fresh install.
    #[test]
    fn the_default_code_font_size_reproduces_the_historical_diff_size() {
        let theme = Theme::dark();
        assert_eq!(
            theme.code_font_size,
            crate::typography::CODE_FONT_SIZE_DEFAULT
        );
        assert_eq!(diff_text_size(&theme), DIFF_TEXT_SIZE);
        assert_eq!(diff_line_height(&theme), DIFF_LINE_HEIGHT);
    }

    #[test]
    fn scaled_diff_sizes_keep_their_proportions_and_stay_clamped() {
        let mut theme = Theme::dark();
        theme.code_font_size = 2.0 * crate::typography::CODE_FONT_SIZE_DEFAULT;
        assert_eq!(diff_text_size(&theme), 2.0 * DIFF_TEXT_SIZE);
        assert_eq!(diff_line_height(&theme), 2.0 * DIFF_LINE_HEIGHT);

        theme.code_font_size = crate::typography::FONT_SIZE_MAX;
        assert!(diff_text_size(&theme) <= crate::typography::FONT_SIZE_MAX);
        assert!(diff_text_size(&theme) >= crate::typography::FONT_SIZE_MIN);
    }

    #[test]
    fn split_line_runs_use_affected_old_and_new_documents() {
        let theme = Theme::dark();
        for (path, source, required) in [
            (
                "src/card.tsx",
                "const view: JSX.Element = <main id=\"app\" />;",
                slopty_syntax::HighlightKind::Tag,
            ),
            (
                "src/Greeter.kt",
                "fun greet(name: String) = println(name)",
                slopty_syntax::HighlightKind::Function,
            ),
            (
                "Dockerfile",
                "RUN echo \"hello\"",
                slopty_syntax::HighlightKind::Function,
            ),
        ] {
            let document = Arc::new(
                slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                    source,
                    path: Some(path),
                    fence_tag: None,
                })
                .unwrap(),
            );
            let highlights = DiffHighlights {
                old: Some(document.clone()),
                new: Some(document),
            };
            for line in [
                DiffLine {
                    kind: LineKind::Del,
                    old_no: Some(1),
                    new_no: None,
                    text: source.into(),
                },
                DiffLine {
                    kind: LineKind::Add,
                    old_no: None,
                    new_no: Some(1),
                    text: source.into(),
                },
            ] {
                assert!(
                    highlights
                        .spans(&line)
                        .iter()
                        .any(|span| span.kind == required),
                    "missing {required:?} for {path} on {:?}",
                    line.kind
                );
                let runs = line_runs(&line, Some(&highlights), &theme);
                assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), source.len());
                assert!(
                    runs.iter()
                        .any(|run| run.color == render::token_color(required, &theme)),
                    "split runs dropped {required:?} for {path} on {:?}",
                    line.kind
                );
            }
        }
    }

    #[test]
    fn excerpt_parses_old_and_new_hunks_as_separate_documents() {
        let file = FileDiff {
            path: "src/lib.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            notices: vec![],
            hunks: vec![Hunk {
                header: "@@ -1,3 +1,3 @@".into(),
                lines: vec![
                    DiffLine {
                        kind: LineKind::Context,
                        old_no: Some(1),
                        new_no: Some(1),
                        text: "/* start".into(),
                    },
                    DiffLine {
                        kind: LineKind::Del,
                        old_no: Some(2),
                        new_no: None,
                        text: "old body".into(),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(2),
                        text: "new body".into(),
                    },
                    DiffLine {
                        kind: LineKind::Context,
                        old_no: Some(3),
                        new_no: Some(3),
                        text: "end */".into(),
                    },
                ],
            }],
            additions: 1,
            deletions: 1,
            max_line: 3,
        };
        let highlights = excerpt_highlights(&file, Lang::Rust).expect("excerpt");
        let deleted = &file.hunks[0].lines[1];
        let added = &file.hunks[0].lines[2];
        assert!(
            highlights
                .spans(deleted)
                .iter()
                .any(|span| span.kind == slopty_syntax::HighlightKind::Comment)
        );
        assert!(
            highlights
                .spans(added)
                .iter()
                .any(|span| span.kind == slopty_syntax::HighlightKind::Comment)
        );
    }

    #[test]
    fn mismatched_full_sources_are_rejected_atomically() {
        let file = FileDiff {
            path: "src/lib.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            notices: vec![],
            hunks: vec![Hunk {
                header: "@@ -1 +1 @@".into(),
                lines: vec![
                    DiffLine {
                        kind: LineKind::Del,
                        old_no: Some(1),
                        new_no: None,
                        text: "let old = 1;".into(),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(1),
                        text: "let new = 2;".into(),
                    },
                ],
            }],
            additions: 1,
            deletions: 1,
            max_line: 1,
        };
        let response = slopty_view_proto::CheckoutFileDiffText {
            diff_checksum: "sum".into(),
            old_text: Some("let old = 1;\n".into()),
            new_text: Some("different snapshot\n".into()),
            old_content_hash: None,
            new_content_hash: None,
            binary: false,
            truncated: false,
            stale: false,
        };
        assert!(!sources_match_patch(&file, &response));
        assert!(full_highlights(&file, Lang::Rust, &response).is_none());
    }
}
