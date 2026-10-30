//! Find in the current thread (⌘F). A field on the transcript's own overlay card,
//! under the titlebar: the tally ("2 of 5"), ↑ and ↓ to step, every match washed where it
//! stands and the current one stronger, landed below the titlebar the way every jump lands.
//! The keys are the shell keymap's ([`FindInThread`], [`FindNext`], [`FindPrevious`]); with no
//! binding of the shell's in the way the actions reach the window's main transcript through the
//! app.
//!
//! It finds over the whole trace (`docs/decisions/inspector.md`, "Keys and find"): the person's
//! prompts and the answers' text, and every step, what its line says and what its detail holds,
//! folded or hidden by the depth as it may be. Landing on a step opens what hides it (the depth,
//! its group, its detail when the match lies there) and puts the trace's cursor on it. Two
//! filters, failed steps only and steps of one kind, share the match: with either on, only the
//! steps it keeps match, each one a match by itself while no words are typed.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, Global, SharedString,
    Subscription, WeakEntity, Window, div, px,
};

use super::trace::Cursor;
use super::{CallKind, Row, RowKind, ToolDetail, ToolItem, ToolItemKind, Transcript, fnv1a};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::markdown::find as paint;
use crate::motion::HoverFade as _;
use crate::popover;
use crate::settings::widgets::{self, ActionHint as _, Keys, SelectOption, SelectState};
use crate::theme::Theme;

gpui::actions!(
    transcript,
    [
        /// Open find in the current thread, or give its field the keyboard.
        FindInThread,
        /// Step to the next match.
        FindNext,
        /// Step to the previous match.
        FindPrevious
    ]
);

/// What kind of step the kind filter keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    Command,
    Edit,
    Read,
    Search,
    Fetch,
    Subagent,
    Thought,
    Other,
}

impl StepKind {
    /// Every kind, in the filter's order.
    pub(crate) const ALL: [Self; 8] = [
        Self::Command,
        Self::Edit,
        Self::Read,
        Self::Search,
        Self::Fetch,
        Self::Subagent,
        Self::Thought,
        Self::Other,
    ];

    /// Its name, as the filter says it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Command => "Commands",
            Self::Edit => "Edits",
            Self::Read => "Reads",
            Self::Search => "Searches",
            Self::Fetch => "Fetches",
            Self::Subagent => "Subagents",
            Self::Thought => "Thinking",
            Self::Other => "Other tools",
        }
    }

    /// The kind of step `tool` is: a listing reads, a plan's update is another tool.
    pub(crate) fn of(tool: &ToolItem) -> Self {
        if tool.kind == ToolItemKind::Thought {
            return Self::Thought;
        }
        match super::call_kind(&tool.call) {
            CallKind::Command => Self::Command,
            CallKind::Edit => Self::Edit,
            CallKind::Listing | CallKind::Read => Self::Read,
            CallKind::Search => Self::Search,
            CallKind::Fetch => Self::Fetch,
            CallKind::Subagent => Self::Subagent,
            CallKind::Todo | CallKind::Other => Self::Other,
        }
    }
}

/// What the filters keep: failed steps only, steps of one kind only, or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    pub(crate) failed: bool,
    pub(crate) kind: Option<StepKind>,
}

impl Filter {
    /// Whether either filter is on, so only the steps they keep match.
    pub(crate) const fn active(self) -> bool {
        self.failed || self.kind.is_some()
    }

    /// Whether the filters keep step `tool`.
    fn keeps(self, tool: &ToolItem) -> bool {
        (!self.failed || super::trace::failed(tool))
            && self.kind.is_none_or(|kind| StepKind::of(tool) == kind)
    }
}

/// One place find counts matches in: a row's text, one step of a group, or a subagent's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) row: SharedString,
    /// The step's part id, when the matches are one step's of a group.
    pub(crate) step: Option<String>,
    /// The matches lie in text the washes paint: a prompt's, an answer's.
    text: bool,
    /// Of the matches, how many lie in what the step's line says; the rest are in its detail.
    in_line: usize,
    pub(crate) count: usize,
}

/// The open find of one transcript.
#[derive(Default)]
pub(crate) struct FindState {
    pub(super) open: bool,
    input: Option<Entity<ComposerInput>>,
    _events: Option<Subscription>,
    /// The words the hits were counted for.
    query: String,
    /// What the filters keep, and what they kept when the hits were counted.
    filter: Filter,
    counted_filter: Filter,
    /// The kind filter's menu.
    kind_menu: SelectState,
    /// Each place with matches, in the order the trace reads.
    hits: Vec<Hit>,
    total: usize,
    /// The current match, by its place among all of them.
    active: Option<usize>,
    /// The rows the hits were counted over.
    signature: u64,
    /// Where the keyboard was before the field took it.
    return_focus: Option<FocusHandle>,
}

impl FindState {
    /// "2 of 5", "No results", or nothing before a word is typed or a filter is on.
    pub(crate) fn tally(&self) -> Option<String> {
        if self.query.is_empty() && !self.counted_filter.active() {
            return None;
        }
        Some(match (self.active, self.total) {
            (_, 0) => "No results".to_owned(),
            (Some(at), total) => format!("{} of {total}", at + 1),
            (None, total) => format!("{total} found"),
        })
    }

    /// The place holding the match at `at`, and the match's place among that place's.
    fn locate(&self, at: usize) -> Option<(&Hit, usize)> {
        let mut seen = 0;
        for hit in &self.hits {
            if at < seen + hit.count {
                return Some((hit, at - seen));
            }
            seen += hit.count;
        }
        None
    }
}

/// The texts a row paints, one per text element, in paint order (what find looks through).
pub(crate) fn row_texts(row: &Row) -> Vec<String> {
    match &row.kind {
        RowKind::User { text, .. } => vec![text.to_string()],
        RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => {
            let mut out = Vec::new();
            if let Some(top) = tree.blocks.get(*block_ix) {
                paint::block_texts(&top.block, &mut out);
            }
            out
        }
        _ => Vec::new(),
    }
}

/// What step `tool`'s line says: its call and its target, and what it came to.
fn step_line(tool: &ToolItem) -> Vec<String> {
    let mut out = vec![super::chip_name(tool).to_string()];
    out.extend(super::chip_result(tool).map(|result| result.to_string()));
    out
}

/// What step `tool`'s detail holds: its whole call, its output, its thought, its diff.
fn step_body(tool: &ToolItem) -> Vec<String> {
    let mut out = Vec::new();
    for detail in [&tool.invocation, &tool.detail].into_iter().flatten() {
        match detail.as_ref() {
            ToolDetail::Output { lines, .. } => out.extend(lines.iter().map(ToString::to_string)),
            ToolDetail::Thought { lines, .. } => {
                out.extend(
                    lines
                        .iter()
                        .map(|runs| runs.iter().map(|run| run.text.as_str()).collect::<String>()),
                );
            }
            ToolDetail::Diff { file, .. } => {
                out.extend(
                    file.hunks
                        .iter()
                        .flat_map(|hunk| hunk.lines.iter().map(|line| line.text.clone())),
                );
            }
            ToolDetail::Stats { stats } => out.extend(stats.iter().map(|stat| stat.path.clone())),
            ToolDetail::Json { lines, .. } => {
                out.extend(lines.iter().map(|line| line.text.clone()))
            }
        }
    }
    out
}

/// Step `tool`'s place, as find counts it in row `row` (`step`: its part id in a group).
fn step_hit(
    row: &SharedString,
    step: Option<&str>,
    tool: &ToolItem,
    needle: &str,
    filter: Filter,
) -> Option<Hit> {
    if !filter.keeps(tool) {
        return None;
    }
    let (in_line, count) = if needle.is_empty() {
        if !filter.active() {
            return None;
        }
        (1, 1)
    } else {
        let in_line = paint::count(&step_line(tool), needle);
        (in_line, in_line + paint::count(&step_body(tool), needle))
    };
    (count > 0).then(|| Hit {
        row: row.clone(),
        step: step.map(str::to_owned),
        text: false,
        in_line,
        count,
    })
}

/// Every place in `rows` with matches of `needle` that `filter` keeps, in order: with no filter,
/// what each row says and each step's line and detail; with one, only the steps it keeps (and,
/// for failed steps alone, the errors the turn met), each a match by itself when `needle` is
/// empty.
pub(crate) fn hits(rows: &[Row], needle: &str, filter: Filter) -> Vec<Hit> {
    let mut out = Vec::new();
    for row in rows {
        match &row.kind {
            RowKind::ToolGroup { tools, .. } => {
                out.extend(tools.iter().filter_map(|tool| {
                    step_hit(&row.id, Some(&tool.part_id), tool, needle, filter)
                }));
            }
            RowKind::Spawn { tool } => out.extend(step_hit(&row.id, None, tool, needle, filter)),
            RowKind::ErrorCard { .. } | RowKind::ErrorChip { .. } if filter.kind.is_none() => {
                let count = if needle.is_empty() {
                    usize::from(filter.failed)
                } else {
                    paint::count(&error_texts(row), needle)
                };
                if count > 0 {
                    out.push(Hit {
                        row: row.id.clone(),
                        step: None,
                        text: false,
                        in_line: count,
                        count,
                    });
                }
            }
            _ if filter.active() || needle.is_empty() => {}
            _ => {
                let count = paint::count(&row_texts(row), needle);
                if count > 0 {
                    out.push(Hit {
                        row: row.id.clone(),
                        step: None,
                        text: true,
                        in_line: count,
                        count,
                    });
                }
            }
        }
    }
    out
}

/// What an error row says.
fn error_texts(row: &Row) -> Vec<String> {
    match &row.kind {
        RowKind::ErrorCard { message, detail } => std::iter::once(message.to_string())
            .chain(detail.as_ref().map(ToString::to_string))
            .collect(),
        RowKind::ErrorChip { message } => vec![message.to_string()],
        _ => Vec::new(),
    }
}

fn rows_signature(rows: &[Row]) -> u64 {
    let mut key = Vec::with_capacity(rows.len() * 24);
    for row in rows {
        key.extend_from_slice(row.id.as_bytes());
        key.extend_from_slice(&row.version.to_le_bytes());
    }
    fnv1a(&key)
}

/// The window's main transcript, for find's keys pressed where no view takes them, and the
/// window it draws in.
struct FindTarget(WeakEntity<Transcript>, Option<gpui::AnyWindowHandle>);

impl Global for FindTarget {}

/// Whether the transcript find's keys reach has its find open: ⇧⌘G steps back in it then, and
/// is Go to Directory otherwise (`shell::picker`).
pub(crate) fn find_open(cx: &App) -> bool {
    cx.try_global::<FindTarget>()
        .and_then(|target| target.0.upgrade())
        .is_some_and(|transcript| transcript.read(cx).find.open)
}

/// The app's find actions are registered.
struct FindActions;

impl Global for FindActions {}

fn with_target(
    cx: &mut App,
    act: impl FnOnce(&mut Transcript, &mut Window, &mut Context<Transcript>) + 'static,
) {
    let Some((target, window)) =
        cx.try_global::<FindTarget>().and_then(|target| Some((target.0.upgrade()?, target.1)))
    else {
        return;
    };
    let Some(window) = window.or_else(|| cx.active_window()) else {
        return;
    };
    // A key the window dispatches reaches this while the window is being updated, which
    // cannot be updated again inside itself: the transcript acts once the dispatch is done.
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| {
            target.update(cx, |transcript, cx| act(transcript, window, cx));
        });
    });
}

impl Transcript {
    /// Make this the transcript find's keys reach, and register them once.
    pub(super) fn install_find(cx: &mut Context<Self>) {
        cx.set_global(FindTarget(cx.weak_entity(), None));
        if cx.has_global::<FindActions>() {
            return;
        }
        cx.set_global(FindActions);
        let app: &mut App = cx;
        app.on_action(|_: &FindInThread, cx| with_target(cx, Self::open_find));
        app.on_action(|_: &FindNext, cx| {
            with_target(cx, |this, window, cx| this.find_step(1, window, cx))
        });
        app.on_action(|_: &FindPrevious, cx| {
            with_target(cx, |this, window, cx| this.find_step(-1, window, cx))
        });
    }

    /// Open find with the selection's words (when there is one) and give it the keyboard.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.input.is_none() {
            let input = cx.new(|cx| {
                ComposerInput::new("Find in session", cx)
                    .with_single_line()
                    .with_accessibility_role(gpui::Role::SearchInput)
                    .with_text_metrics(12.5, 18.0)
            });
            let events =
                cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                    ComposerInputEvent::Edited => {
                        this.find.active = None;
                        this.refresh_find(cx);
                        if this.find.total > 0 {
                            this.find_step(1, window, cx);
                        }
                        cx.notify();
                    }
                    ComposerInputEvent::Submitted | ComposerInputEvent::MentionAccept => {
                        this.find_step(1, window, cx)
                    }
                    ComposerInputEvent::MentionNavigate(delta) => {
                        this.find_step(*delta, window, cx)
                    }
                    ComposerInputEvent::MentionDismiss => this.close_find(window, cx),
                    _ => {}
                });
            self.find.input = Some(input);
            self.find._events = Some(events);
        }
        let Some(input) = self.find.input.clone() else {
            return;
        };
        if !self.find.open {
            self.find.return_focus = window.focused(cx);
            self.find.open = true;
            if let Some(selected) = crate::markdown::selection::selected_text()
                .map(|text| slopty_view_proto::view::single_line(&text))
                .filter(|text| !text.is_empty() && text.chars().count() <= 200)
            {
                input.update(cx, |input, cx| input.set_text(selected, cx));
            }
        }
        input.update(cx, |input, cx| input.select_all_text(cx));
        window.focus(&input.focus_handle(cx), cx);
        self.refresh_find(cx);
        cx.notify();
    }

    /// Close find, take its washes away and give the keyboard back.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        self.find.open = false;
        self.find.active = None;
        paint::clear(cx.entity_id().as_u64());
        if let Some(focus) = self.find.return_focus.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Step `delta` matches on (wrapping), and land the match's row below the titlebar.
    pub(crate) fn find_step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            self.open_find(window, cx);
            return;
        }
        self.refresh_find(cx);
        let total = self.find.total;
        if total == 0 {
            return;
        }
        let next = match self.find.active {
            None if delta < 0 => total - 1,
            None => 0,
            Some(at) => (at as isize + delta).rem_euclid(total as isize) as usize,
        };
        self.find.active = Some(next);
        self.publish_find(cx);
        if let Some((hit, place)) = self.find.locate(next).map(|(hit, place)| (hit.clone(), place))
        {
            // A step's match lies in its detail when it is past what the step's line says.
            let detail = hit.step.is_some() && place >= hit.in_line;
            let cursor = Cursor { row: hit.row.clone(), step: hit.step.clone() };
            if let Some(ix) = self.open_to(cursor, detail, cx) {
                // A text's match marks the landing; anything else flashes its row.
                self.glide_to_row(ix, !hit.text, cx);
            }
        }
        cx.notify();
    }

    /// Set what the filters keep and land on the first step they keep.
    pub(super) fn set_find_filter(
        &mut self,
        filter: Filter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find.filter == filter {
            return;
        }
        self.find.filter = filter;
        self.find.active = None;
        self.refresh_find(cx);
        if self.find.total > 0 {
            self.find_step(1, window, cx);
        }
        cx.notify();
    }

    /// Count the matches again when the words or the rows changed.
    fn refresh_find(&mut self, cx: &mut Context<Self>) {
        let query = self
            .find
            .input
            .as_ref()
            .map(|input| input.read(cx).text().to_owned())
            .unwrap_or_default();
        // The whole trace, what the depth hides included.
        let rows = self.whole_rows();
        let signature = rows_signature(rows);
        let filter = self.find.filter;
        if query == self.find.query
            && filter == self.find.counted_filter
            && signature == self.find.signature
        {
            return;
        }
        let kept = self
            .find
            .active
            .filter(|_| query == self.find.query && filter == self.find.counted_filter)
            .and_then(|at| self.find.locate(at))
            .map(|(hit, place)| (hit.row.clone(), hit.step.clone(), place));
        self.find.hits = hits(rows, &query, filter);
        self.find.total = self.find.hits.iter().map(|hit| hit.count).sum();
        self.find.query = query;
        self.find.counted_filter = filter;
        self.find.signature = signature;
        // The current match stays the same one while rows come and go around it.
        self.find.active = kept.and_then(|(row, step, place)| {
            let mut seen = 0;
            for hit in &self.find.hits {
                if hit.row == row && hit.step == step {
                    return Some(seen + place.min(hit.count.saturating_sub(1)));
                }
                seen += hit.count;
            }
            None
        });
        self.publish_find(cx);
    }

    /// Tell the rows what to wash.
    fn publish_find(&self, cx: &mut Context<Self>) {
        let surface = cx.entity_id().as_u64();
        if !self.find.open || self.find.query.is_empty() {
            paint::clear(surface);
            return;
        }
        // The washes are the texts'; a step's match is marked by the cursor on it.
        let active = self
            .find
            .active
            .and_then(|at| self.find.locate(at))
            .filter(|(hit, _)| hit.text)
            .map(|(hit, place)| (hit.row.to_string(), place));
        paint::set(
            surface,
            paint::FindPaint {
                needle: self.find.query.clone(),
                rows: self
                    .find
                    .hits
                    .iter()
                    .filter(|hit| hit.text)
                    .map(|hit| hit.row.to_string())
                    .collect(),
                active,
                washes: paint::washes(Theme::of(cx)),
            },
        );
    }

    /// Whether find is open, its card over the transcript's top right.
    pub(super) fn finding(&self) -> bool {
        self.find.open
    }

    /// The find card over the transcript's top right, while find is open.
    pub(super) fn render_find_bar(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.note_find_window(window, cx);
        if !self.find.open {
            return None;
        }
        self.refresh_find(cx);
        let input = self.find.input.clone()?;
        let theme = Theme::of(cx).clone();
        let tally = self.find.tally();
        let has = self.find.total > 0;
        // At the top of the pane the transcript lays out in, which clears the window's
        // chrome itself.
        let top = Theme::SPACE_SM;
        // The house's find card (the diff's and the terminal's anatomy): its steps name
        // their keys, a step with nowhere to go dims, and it enters as a menu does. Under the
        // field, the filters that share its match.
        let field = div()
            .h(px(popover::SEARCH_ROW_HEIGHT))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                crate::icons::icon(crate::icons::MAGNIFER)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(div().flex_1().min_w_0().child(input))
            .children(tally.map(|tally| popover::find_tally(&theme, tally)))
            .child(
                popover::find_step("find-previous", crate::icons::ALT_ARROW_UP, has, &theme)
                    .action_hint("Previous match", Keys::Action(Box::new(FindPrevious)), cx)
                    .when(has, |el| {
                        el.on_click(
                            cx.listener(|this, _, window, cx| this.find_step(-1, window, cx)),
                        )
                    }),
            )
            .child(
                popover::find_step("find-next", crate::icons::ALT_ARROW_DOWN, has, &theme)
                    .action_hint("Next match", Keys::Action(Box::new(FindNext)), cx)
                    .when(has, |el| {
                        el.on_click(
                            cx.listener(|this, _, window, cx| this.find_step(1, window, cx)),
                        )
                    }),
            )
            .child(
                popover::find_step("find-close", crate::icons::CLOSE, true, &theme)
                    .action_hint("Close find", Keys::Fixed("escape"), cx)
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            );
        let filter = self.find.filter;
        let kinds = std::iter::once(SelectOption::new("Every kind"))
            .chain(StepKind::ALL.iter().map(|kind| SelectOption::new(kind.name())));
        let kind_at = filter
            .kind
            .and_then(|kind| StepKind::ALL.iter().position(|k| *k == kind))
            .map_or(0, |at| at + 1);
        let filters = div()
            .px(px(8.0))
            .pb(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(failed_toggle(filter.failed, &theme).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.set_find_filter(Filter { failed: !filter.failed, ..filter }, window, cx);
                },
            )))
            .child(
                widgets::select("find-kind", "Kind", &theme, |this: &mut Self| {
                    &mut this.find.kind_menu
                })
                .options(kinds, kind_at)
                .on_select(move |this, at, window, cx| {
                    let kind = at.checked_sub(1).and_then(|at| StepKind::ALL.get(at).copied());
                    this.set_find_filter(Filter { kind, ..this.find.filter }, window, cx);
                })
                .render(&self.find.kind_menu, cx),
            );
        let card = popover::popover_card_flush(&theme)
            .id("find-in-thread")
            .w(px(popover::FIND_CARD_WIDTH))
            .flex()
            .flex_col()
            .role(gpui::Role::Search)
            .aria_label("Find in session")
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_find(window, cx);
                }
            }))
            .child(field)
            .child(filters);
        Some(
            div()
                .absolute()
                .top(px(top))
                .right(px(Theme::SPACE_LG + self.right_reserve))
                .child(popover::find_card_in("find-in-thread-in", card))
                .into_any_element(),
        )
    }

    /// Find's keys go to the window this transcript draws in, whether or not that
    /// window is the key one (`cx.active_window()` is `None` then).
    fn note_find_window(&self, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let me = cx.entity_id();
        let stale = cx
            .try_global::<FindTarget>()
            .is_some_and(|target| target.0.entity_id() == me && target.1 != Some(handle));
        if stale {
            cx.global_mut::<FindTarget>().1 = Some(handle);
        }
    }
}

/// The filter to failed steps alone: a pill of the dropdowns' height and fill, its glyph in the
/// danger hue while it is on, the field keeping the keyboard when it is pressed.
fn failed_toggle(on: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let hover: SharedString = "find-failed-hover".into();
    let toggle = div()
        .id("find-failed")
        .flex_none()
        .h(px(widgets::SELECT_HEIGHT))
        .px(px(10.0))
        .rounded(px(8.0))
        .bg(if on {
            crate::theme::selected_wash()
        } else {
            crate::motion::hover_blend(
                &hover,
                widgets::select_fill(theme, false),
                widgets::select_fill(theme, true),
            )
        })
        .hover_fade(hover)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(12.5))
        .text_color(if on { theme.text } else { theme.text_muted })
        .tab_index(0)
        .role(gpui::Role::Switch)
        .aria_label("Failed steps only")
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
        .child(crate::icons::icon(crate::icons::DANGER_TRIANGLE).size(px(13.0)).text_color(if on {
            theme.danger
        } else {
            theme.text_muted
        }))
        .child("Failed");
    crate::surface_chrome::focus_ring(toggle, theme)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn markdown(id: &str, text: &str) -> Row {
        Row {
            id: id.to_owned().into(),
            version: 0,
            turn_start: false,
            kind: RowKind::Markdown {
                tree: Arc::new(crate::markdown::parser::parse_full(text)),
                block_ix: 0,
            },
            entry_id: "e".into(),
            timestamp: None,
            copy_text: None,
            compact_fold: None,
        }
    }

    fn call(
        id: &str,
        call: slopty_view_proto::ToolCall,
        failed: bool,
        output: &[&str],
    ) -> ToolItem {
        ToolItem {
            part_id: id.into(),
            call,
            is_error: failed,
            resolved: true,
            detail: (!output.is_empty()).then(|| {
                Arc::new(ToolDetail::Output {
                    lines: output
                        .iter()
                        .map(|line| SharedString::from((*line).to_owned()))
                        .collect(),
                    truncated_by: 0,
                    stderr_from: None,
                })
            }),
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            subagent_ref: None,
            subagent_status: None,
            kind: ToolItemKind::Call,
            output_text: None,
            images: Arc::new([]),
            facts: None,
        }
    }

    fn group(id: &str, tools: Vec<ToolItem>) -> Row {
        Row {
            kind: RowKind::ToolGroup { summary: "Ran 3 commands".into(), tools: Arc::new(tools) },
            ..markdown(id, "")
        }
    }

    #[test]
    fn find_counts_and_steps_matches() {
        let rows = vec![
            markdown("a", "The cat sat."),
            markdown("b", "No match here."),
            markdown("c", "Cat and CAT, again a cat."),
        ];
        let hits = hits(&rows, "cat", Filter::default());
        let counts: Vec<(&str, usize)> =
            hits.iter().map(|hit| (hit.row.as_ref(), hit.count)).collect();
        assert_eq!(counts, [("a", 1), ("c", 3)]);
        let mut state =
            FindState { open: true, query: "cat".into(), total: 4, hits, ..Default::default() };
        assert_eq!(state.tally().as_deref(), Some("4 found"));
        state.active = Some(1);
        assert_eq!(state.tally().as_deref(), Some("2 of 4"));
        let (hit, place) = state.locate(3).unwrap();
        assert_eq!((hit.row.as_ref(), place), ("c", 2));
        assert_eq!(state.locate(4), None);
        state.total = 0;
        assert_eq!(state.tally().as_deref(), Some("No results"));
    }

    /// Find looks through every step of a group, folded as it is: what its line says, then what
    /// its detail holds. The filters keep failed steps alone or steps of one kind, the words then
    /// counted in those alone, and each kept step is a match by itself while no words are typed.
    #[test]
    fn find_looks_through_steps_and_its_filters_share_the_match() {
        use slopty_view_proto::ToolCall;
        let rows = vec![
            markdown("a", "The parser is next."),
            group(
                "g",
                vec![
                    call(
                        "t1",
                        ToolCall::Exec { command: "cargo test parser".into() },
                        false,
                        &["test parser ... ok"],
                    ),
                    call(
                        "t2",
                        ToolCall::Exec { command: "cargo build".into() },
                        true,
                        &["error: parser failed"],
                    ),
                    call("t3", ToolCall::ReadFile { path: "src/parser.rs".into() }, false, &[]),
                ],
            ),
        ];
        let places = |hits: &[Hit]| -> Vec<(String, Option<String>, usize, usize)> {
            hits.iter()
                .map(|hit| (hit.row.to_string(), hit.step.clone(), hit.in_line, hit.count))
                .collect()
        };
        let step = |id: &str| Some(id.to_owned());
        assert_eq!(
            places(&hits(&rows, "parser", Filter::default())),
            [
                ("a".into(), None, 1, 1),
                ("g".into(), step("t1"), 1, 2),
                ("g".into(), step("t2"), 0, 1),
                ("g".into(), step("t3"), 1, 1),
            ],
            "the words, then each step's line and detail"
        );
        let failed = Filter { failed: true, kind: None };
        assert_eq!(
            places(&hits(&rows, "parser", failed)),
            [("g".into(), step("t2"), 0, 1)],
            "failed steps alone"
        );
        assert_eq!(
            places(&hits(&rows, "", failed)),
            [("g".into(), step("t2"), 1, 1)],
            "each a match by itself"
        );
        let reads = Filter { failed: false, kind: Some(StepKind::Read) };
        assert_eq!(
            places(&hits(&rows, "", reads)),
            [("g".into(), step("t3"), 1, 1)],
            "a kind alone"
        );
        assert!(hits(&rows, "", Filter::default()).is_empty(), "nothing before a word or a filter");
        let state = FindState { counted_filter: failed, ..Default::default() };
        assert_eq!(state.tally().as_deref(), Some("No results"), "a filter on says its tally");
    }

    /// ⌘F reaches the window's transcript from a key the window dispatches, the keyboard
    /// elsewhere and the window not the key one: find opens and takes the keyboard.
    #[gpui::test]
    fn find_opens_from_the_windows_keys_key_or_not(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        struct Fixture(Entity<Transcript>, gpui::FocusHandle);
        impl gpui::Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gpui::div().size_full().track_focus(&self.1).child(self.0.clone())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(true);
        });
        let state = cx.new(|_| crate::state::AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        let (fixture, cx) =
            cx.add_window_view(|_, cx| Fixture(transcript.clone(), cx.focus_handle()));
        let elsewhere = fixture.read_with(cx, |fixture, _| fixture.1.clone());
        cx.update(|window, cx| window.focus(&elsewhere, cx));
        cx.run_until_parked();
        cx.deactivate_window();
        cx.update(|window, cx| window.dispatch_action(Box::new(FindInThread), cx));
        cx.run_until_parked();
        assert!(transcript.read_with(cx, |this, _| this.finding()), "find opened");
        let keyboard = cx.update(|window, cx| {
            transcript
                .read(cx)
                .find
                .input
                .as_ref()
                .is_some_and(|input| input.focus_handle(cx).is_focused(window))
        });
        assert!(keyboard, "and has the keyboard");
    }
}
