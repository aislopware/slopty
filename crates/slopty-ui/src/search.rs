//! Search in files: a query typed once, every matching line under a directory on a worker,
//! grouped by file as the worker finds them.
//!
//! [`ProjectSearch`] floats over the workspace as the palette does, at the editor's width since
//! its rows are lines of code. The field searches as it is typed (after [`DEBOUNCE`]), each
//! change stopping the search before it on the worker ([`slopty_proto::search`]); the files
//! field narrows it with ripgrep's globs, and three toggles (match case, whole word, regular
//! expression) set how it matches. A file's row holds its name, where it is and how many lines
//! matched, a click folding its lines away; a line's row holds its number and its text with
//! each match tinted. ↑/↓ move, ↩ or a click opens the file tile at the line, Esc closes.
//! Closing stops a search still going; the workspace keeps the surface, so it opens again as
//! it was left and runs a stopped search again.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, HighlightStyle, InteractiveElement as _, IntoElement, KeyContext, MouseButton,
    ParentElement as _, Render, ScrollStrategy, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledText, Subscription, Task, UniformListScrollHandle, Window, div, px,
    uniform_list,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use slopty_client::search::{Row, SearchResults, SearchState};
use slopty_proto::search::{FileHits, LineHit, MAX_LINES, SearchEvent, SearchQuery};
use slopty_proto::{ClientMsg, RequestId};
use slopty_theme::{Theme, Typography, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize};
use crate::kit::Pace;
use crate::palette::{Layer, Plate};

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        project_search,
        [
            /// Match upper and lower case apart, or let them match each other.
            ToggleMatchCase,
            /// Match whole words only, or any text.
            ToggleWholeWord,
            /// Read the query as a regular expression, or as the text it is.
            ToggleRegex,
        ]
    );
}
pub use actions::{ToggleMatchCase, ToggleRegex, ToggleWholeWord};

/// The key context of the surface; its toggles' keys are bound in it.
pub const CTX: &str = "ProjectSearch";

/// What the query field says before anything is typed.
pub(crate) const QUERY_PLACEHOLDER: &str = "Search in files";
/// What the files field says before anything is typed.
pub(crate) const FILES_PLACEHOLDER: &str = "Files to include, as *.rs or !tests/**";
/// The toggles' names, as a screen reader and the palette say them.
pub(crate) const MATCH_CASE: &str = "Match case";
pub(crate) const WHOLE_WORD: &str = "Match whole word";
pub(crate) const REGEX: &str = "Use regular expression";
/// What the list says while a search has found nothing yet.
pub(crate) const SEARCHING: &str = "Searching\u{2026}";
/// What the list says when a search went through everything and found nothing.
pub(crate) const NO_RESULTS: &str = "No results";

/// How long the field waits after a keystroke before it searches: a word typed is one search,
/// not one per letter.
pub const DEBOUNCE: Duration = Duration::from_millis(120);

/// The width of a line row's number column at zoom 1, room for five figures.
const NUMBER_W: f32 = 40.0;

/// The most of the window's height the surface takes, under its ceiling.
const SHARE: f32 = 0.7;

/// Numbers searches for every surface in the app, so a page still in flight for a closed one
/// is never taken for a new one's.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// What the surface asks of the workspace.
#[derive(Clone, PartialEq, Debug)]
pub enum ProjectSearchEvent {
    /// Send this to the surface's worker: a search's start or stop.
    Send(ClientMsg),
    /// Open the file at `path` on the worker, landing on `line` (from 1).
    Open {
        /// Worker path.
        path: String,
        /// Line, from 1.
        line: u32,
    },
    /// Esc, or a click outside.
    Dismiss,
}

impl EventEmitter<ProjectSearchEvent> for ProjectSearch {}

/// A row as the selection holds it across pages: the file's path, and which of its lines.
type Selected = (String, Option<usize>);

/// The surface.
pub struct ProjectSearch {
    query: Entity<InputState>,
    files: Entity<InputState>,
    match_case: bool,
    whole_word: bool,
    regex: bool,
    /// The directory searched, as the worker is asked it.
    root: String,
    /// Where that is, as the surface says it (`~/w/slopty on studio`).
    place: String,
    results: Option<SearchResults>,
    rows: Vec<Row>,
    folded: HashSet<String>,
    selected: Option<Selected>,
    /// The selection was moved by a key or the pointer; until then it follows the first
    /// match, wherever the pages put it.
    chosen: bool,
    scroll: UniformListScrollHandle,
    plate: Plate,
    /// The search the last keystroke asked for, waiting out [`DEBOUNCE`].
    pending: Option<Task<()>>,
    theme: Theme,
    _events: [Subscription; 2],
}

impl std::fmt::Debug for ProjectSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectSearch")
            .field("root", &self.root)
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Focusable for ProjectSearch {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl ProjectSearch {
    /// A surface searching `root` on a worker, `place` saying where that is, its field empty;
    /// [`Self::seed`] starts it on a needle.
    pub fn new(
        root: &str,
        place: &str,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(QUERY_PLACEHOLDER));
        let files = cx.new(|cx| InputState::new(window, cx).placeholder(FILES_PLACEHOLDER));
        let field = |this: &mut Self, event: &InputEvent, cx: &mut Context<Self>| match event {
            InputEvent::Change => this.changed(cx),
            InputEvent::PressEnter { .. } => this.enter(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        };
        let events = [
            cx.subscribe(&query, move |this, _input, event, cx| field(this, event, cx)),
            cx.subscribe(&files, move |this, _input, event, cx| field(this, event, cx)),
        ];
        Self {
            query,
            files,
            match_case: false,
            whole_word: false,
            regex: false,
            root: root.to_owned(),
            place: place.to_owned(),
            results: None,
            rows: Vec::new(),
            folded: HashSet::new(),
            selected: None,
            chosen: false,
            scroll: UniformListScrollHandle::new(),
            plate: Plate::default(),
            pending: None,
            theme,
            _events: events,
        }
    }

    /// Put `text` in the field, selected, and search for it now; nothing for an empty one.
    pub fn seed(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        self.query.update(cx, |input, cx| {
            input.set_value(text.to_owned(), window, cx);
            input.select_all(window, cx);
        });
        self.run(cx);
    }

    /// The directory it searches, as the worker is asked it.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The query as the fields and the toggles stand.
    #[must_use]
    pub fn query(&self, cx: &App) -> SearchQuery {
        SearchQuery {
            pattern: self.query.read(cx).value().to_string(),
            regex: self.regex,
            match_case: self.match_case,
            whole_word: self.whole_word,
            globs: slopty_client::search::globs(&self.files.read(cx).value()),
        }
    }

    /// The search shown, once one ran.
    #[must_use]
    pub const fn results(&self) -> Option<&SearchResults> {
        self.results.as_ref()
    }

    /// The selected row's file and line, as the list shows them.
    #[must_use]
    pub fn selected(&self) -> Option<(&FileHits, Option<&LineHit>)> {
        let (path, line) = self.selected.as_ref()?;
        let file = self.results.as_ref()?.files().iter().find(|f| &f.path == path)?;
        Some((file, line.and_then(|l| file.lines.get(l))))
    }

    /// Give the query field the keyboard, its text selected.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// A field changed: search once the typing pauses.
    fn changed(&mut self, cx: &Context<Self>) {
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _gone = this.update(cx, |this, cx| {
                this.pending = None;
                this.run(cx);
            });
        }));
    }

    /// ↩: a search the typing has not started yet starts now; else the selected line opens.
    fn enter(&mut self, cx: &mut Context<Self>) {
        if self.pending.take().is_some() || self.stale(cx) {
            self.run(cx);
        } else {
            self.open_selected(cx);
        }
    }

    /// Whether what is shown is not what the fields ask for now.
    fn stale(&self, cx: &App) -> bool {
        self.results.as_ref().is_none_or(|r| *r.query() != self.query(cx))
    }

    /// Search for what the fields say, unless that is what is shown: the worker stops the
    /// search before it on its own. An empty field clears the list and stops it.
    pub fn run(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        let query = self.query(cx);
        if query.pattern.is_empty() {
            self.stop(cx);
            self.results = None;
            self.refresh();
            cx.notify();
            return;
        }
        if !self.stale(cx) {
            return;
        }
        let id: RequestId = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (results, request) = SearchResults::start(id, &self.root, query);
        tracing::debug!(id, root = %self.root, "search in files");
        self.results = Some(results);
        self.selected = None;
        self.chosen = false;
        self.refresh();
        cx.emit(ProjectSearchEvent::Send(request));
        cx.notify();
    }

    /// Stop the search if it is still going: the surface closed.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        if let Some(stop) = self.results.as_mut().and_then(SearchResults::stop) {
            cx.emit(ProjectSearchEvent::Send(stop));
        }
    }

    /// The surface is shown again: a search its closing stopped runs again.
    pub fn resume(&mut self, cx: &mut Context<Self>) {
        if self.results.as_ref().is_some_and(|r| *r.state() == SearchState::Stopped) {
            self.results = None;
            self.run(cx);
        }
    }

    /// A page or the end of a search from the worker; another search's is dropped.
    pub fn apply(&mut self, event: SearchEvent, cx: &mut Context<Self>) {
        let Some(results) = self.results.as_mut() else { return };
        if results.apply(event) {
            self.refresh();
            cx.notify();
        }
    }

    /// Lay the rows out again. The selection stays on the row it was moved to; until it is
    /// moved it is the first match, which a later page can change, so ↩ opens the top one.
    fn refresh(&mut self) {
        let Some(results) = &self.results else {
            self.rows.clear();
            self.selected = None;
            return;
        };
        self.rows = results.rows(|path| self.folded.contains(path));
        let kept =
            self.chosen && self.selected.as_ref().is_some_and(|s| self.index_of(s).is_some());
        if !kept {
            self.selected =
                self.rows.iter().find(|r| matches!(r, Row::Line { .. })).map(|r| self.key(*r));
            if let Some(ix) = self.selected_index() {
                self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        }
    }

    fn key(&self, row: Row) -> Selected {
        let files = self.results.as_ref().map_or(&[][..], SearchResults::files);
        match row {
            Row::File(file) => (files.get(file).map(|f| f.path.clone()).unwrap_or_default(), None),
            Row::Line { file, line } => {
                (files.get(file).map(|f| f.path.clone()).unwrap_or_default(), Some(line))
            }
        }
    }

    fn index_of(&self, selected: &Selected) -> Option<usize> {
        self.rows.iter().position(|r| self.key(*r) == *selected)
    }

    fn selected_index(&self) -> Option<usize> {
        self.index_of(self.selected.as_ref()?)
    }

    /// Move the selection `delta` rows, stopping at the ends.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(last) = self.rows.len().checked_sub(1) else { return };
        let at = self.selected_index().map_or(0, |ix| ix.saturating_add_signed(delta).min(last));
        self.select(at, cx);
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).copied() else { return };
        self.chosen = true;
        self.selected = Some(self.key(row));
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Open what is selected: a line at itself, a file at its first match.
    fn open_selected(&self, cx: &mut Context<Self>) {
        let Some(results) = &self.results else { return };
        let Some((file, line)) = self.selected() else { return };
        let Some(hit) = line.or_else(|| file.lines.first()) else { return };
        let path = results.path_of(file);
        tracing::info!(%path, line = hit.line, "search opens a match");
        cx.emit(ProjectSearchEvent::Open { path, line: hit.line });
    }

    /// Fold a file's lines away, or unfold them.
    fn fold(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.folded.remove(path) {
            self.folded.insert(path.to_owned());
        }
        self.refresh();
        cx.notify();
    }

    fn toggle(&mut self, which: fn(&mut Self) -> &mut bool, cx: &mut Context<Self>) {
        let on = which(self);
        *on = !*on;
        self.run(cx);
        cx.notify();
    }

    /// Row `ix`: a file's heading or one of its lines.
    fn row(&self, ix: usize, cx: &Context<Self>) -> Option<AnyElement> {
        let row = *self.rows.get(ix)?;
        let results = self.results.as_ref()?;
        let chosen = self.selected_index() == Some(ix);
        let theme = &self.theme;
        let s = theme.surfaces;
        let pad = crate::palette::list_pad(theme);
        let (content, label) = match row {
            Row::File(file) => {
                let hits = results.files().get(file)?;
                (self.file_row(hits), format!("{}, {}", hits.path, count_label(hits.lines.len())))
            }
            Row::Line { file, line } => {
                let hits = results.files().get(file)?;
                let hit = hits.lines.get(line)?;
                (self.line_row(hit), format!("Line {}: {}", hit.line, hit.text))
            }
        };
        let el = div()
            .id(ElementId::NamedInteger("search-row".into(), u64::try_from(ix).unwrap_or(0)))
            .debug_selector(move || format!("search-row-{ix}"))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(label))
            .aria_selected(chosen)
            .w_full()
            .h(px(crate::palette::line_height(theme)))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .px(px(theme.spacing.inset() - pad))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .active(move |st| st.bg(hsla(s.overlay)))
            .on_mouse_move(cx.listener(move |this, _ev, _window, cx| {
                if this.selected_index() != Some(ix) {
                    this.select(ix, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.select(ix, cx);
                match row {
                    Row::File(_) => {
                        let path = this.key(row).0;
                        this.fold(&path, cx);
                    }
                    Row::Line { .. } => this.open_selected(cx),
                }
            }))
            .child(content);
        Some(if chosen {
            self.plate.mark(el, ix).into_any_element()
        } else {
            el.into_any_element()
        })
    }

    /// A file's heading: a chevron that says whether its lines show, its name, the folder it
    /// is in, and how many lines matched.
    fn file_row(&self, hits: &FileHits) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (dir, name) = hits.path.rsplit_once('/').unwrap_or(("", hits.path.as_str()));
        let chevron = if self.folded.contains(&hits.path) {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .child(crate::icons::icon(theme, chevron, IconSize::Inline, hsla(s.text_muted)))
            .child(
                div()
                    .flex_none()
                    .max_w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                crate::kit::meta(div(), theme)
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(dir.to_owned())),
            )
            .child(
                crate::kit::meta(crate::kit::tabular(div()), theme)
                    .flex_none()
                    .child(SharedString::from(hits.lines.len().to_string())),
            )
            .into_any_element()
    }

    /// A matching line: its number in a column of its own, then its text with each match
    /// tinted as a find bar tints its hits, an ellipsis where the line was cut.
    fn line_row(&self, hit: &LineHit) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (text, marks) = shown_line(hit);
        let tint = HighlightStyle {
            color: Some(hsla(s.text)),
            background_color: Some(hsla_alpha(s.warn, alpha::FAINT)),
            font_weight: Some(gpui::FontWeight(Typography::MEDIUM_WEIGHT)),
            ..HighlightStyle::default()
        };
        let highlights: Vec<_> = marks.into_iter().map(|range| (range, tint)).collect();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .child(
                crate::kit::meta(crate::kit::tabular(div()), theme)
                    .flex_none()
                    .w(px(NUMBER_W))
                    .text_right()
                    .child(SharedString::from(hit.line.to_string())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text_secondary))
                    .child(StyledText::new(text).with_highlights(highlights)),
            )
            .into_any_element()
    }

    /// The field and the toggles, then the files field and where the search looks.
    fn fields(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let toggle = |id: &'static str, icon, label, on, act: fn(&mut Self) -> &mut bool| {
            crate::kit::icon_toggle(theme, id, icon, label, on, 1.0)
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _ev, _window, cx| this.toggle(act, cx)))
        };
        let query_row = crate::kit::inset_x(div(), theme)
            .flex_none()
            .h(px(theme.density.row + theme.spacing.lg))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.query)
                        .appearance(false)
                        .px_0()
                        .text_size(px(theme.typography.title()))
                        .aria_label(QUERY_PLACEHOLDER),
                ),
            )
            .child(toggle(
                "search-case",
                IconName::CaseSensitive,
                MATCH_CASE,
                self.match_case,
                |t| &mut t.match_case,
            ))
            .child(toggle("search-word", IconName::WholeWord, WHOLE_WORD, self.whole_word, |t| {
                &mut t.whole_word
            }))
            .child(toggle("search-regex", IconName::Regex, REGEX, self.regex, |t| &mut t.regex));
        let files_row = crate::kit::inset_x(div(), theme)
            .flex_none()
            .h(px(theme.density.row))
            .flex()
            .items_center()
            .gap(px(theme.spacing.md))
            .border_b_1()
            .border_color(hsla(s.border_subtle))
            .text_size(px(theme.typography.small()))
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.files)
                        .appearance(false)
                        .px_0()
                        .text_size(px(theme.typography.small()))
                        .aria_label("Files to include"),
                ),
            )
            .child(
                crate::kit::meta(div(), theme)
                    .id("search-place")
                    .debug_selector(|| "search-place".to_owned())
                    .role(Role::Label)
                    .aria_label(SharedString::from(self.place.clone()))
                    .flex_none()
                    .max_w(px(crate::kit::Overlay::Editor.bounds().0 / 2.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(self.place.clone())),
            );
        div().flex_none().flex().flex_col().child(query_row).child(files_row).into_any_element()
    }

    /// What the foot says of the search: how many matches in how many files, that it stopped
    /// at the cap, or why it could not run.
    fn status(&self) -> Option<(String, bool)> {
        let results = self.results.as_ref()?;
        let files = results.files().len();
        let found = format!("{} in {}", result_label(results.lines()), file_label(files));
        Some(match results.state() {
            SearchState::Running if files == 0 => (SEARCHING.to_owned(), false),
            SearchState::Done(summary) if summary.capped => (
                format!(
                    "First {} results. Narrow the search to see the rest",
                    thousands(MAX_LINES)
                ),
                true,
            ),
            SearchState::Done(_) if files == 0 => (NO_RESULTS.to_owned(), false),
            SearchState::Failed(error) => (error.clone(), true),
            SearchState::Running | SearchState::Stopped | SearchState::Done(_) => (found, false),
        })
    }

    /// The foot: the search's state on the left, the keys on the right.
    fn foot(&self) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (said, warn) = self.status().unwrap_or_default();
        let running = self.results.as_ref().is_some_and(SearchResults::running);
        let key = |key: &'static str, what: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs + theme.spacing.xxs))
                .child(crate::kit::key_cap(theme, key))
                .child(what)
        };
        crate::kit::inset_x(div(), theme)
            .id("search-foot")
            .debug_selector(|| "search-foot".to_owned())
            .flex_none()
            .h(px(crate::palette::line_height(theme)))
            .flex()
            .items_center()
            .gap(px(theme.spacing.md))
            .bg(hsla(s.raised))
            .rounded_b(px(theme.radii.lg - 1.0))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .when(running, |el| {
                el.child(crate::icons::status_icon(
                    theme,
                    crate::icons::Status::Working,
                    px(theme.typography.icon()),
                    hsla(s.accent),
                ))
            })
            .child(
                crate::kit::tabular(div())
                    .id("search-status")
                    .debug_selector(|| "search-status".to_owned())
                    .role(Role::Status)
                    .aria_label(SharedString::from(said.clone()))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .when(warn, |el| el.text_color(hsla(s.warn)))
                    .child(SharedString::from(said)),
            )
            .child(key("↑↓", "Move"))
            .child(key("↩", "Open").text_color(hsla(s.text_secondary)))
            .into_any_element()
    }

    /// The list, drawn as far as it is seen, or one quiet line for why there is none.
    fn list(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let pad = crate::palette::list_pad(theme);
        if self.rows.is_empty() {
            let text = match self.results.as_ref().map(SearchResults::state) {
                Some(SearchState::Running) => Some(SEARCHING),
                Some(SearchState::Done(_)) => Some(NO_RESULTS),
                _ => None,
            };
            return div()
                .flex_none()
                .children(text.map(|text| {
                    crate::kit::inset_x(div(), theme)
                        .id("search-empty")
                        .debug_selector(|| "search-empty".to_owned())
                        .role(Role::Status)
                        .aria_label(text)
                        .py(px(theme.spacing.sm))
                        .text_size(px(theme.typography.small()))
                        .text_color(hsla(theme.surfaces.text_muted))
                        .child(text)
                }))
                .into_any_element();
        }
        let rows = uniform_list(
            "search-rows",
            self.rows.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.filter_map(|ix| this.row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full()
        .p(px(pad));
        div()
            .id("search-list")
            .debug_selector(|| "search-list".to_owned())
            .role(Role::ListBox)
            .aria_label("Results")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.plate.under(theme))
            .child(rows)
            .into_any_element()
    }
}

impl Render for ProjectSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let height = f32::from(window.viewport_size().height);
        let (_, ceiling) = crate::kit::Overlay::Editor.bounds();
        let mut context = KeyContext::new_with_defaults();
        context.add(CTX);
        let panel = crate::kit::dialog(&theme, crate::kit::Overlay::Editor)
            .id("search")
            .debug_selector(|| "search".to_owned())
            .role(Role::Dialog)
            .aria_label(QUERY_PLACEHOLDER)
            .key_context(context)
            .max_h(px(ceiling.min(height * SHARE)))
            .when(!self.rows.is_empty(), |el| el.h(px(ceiling.min(height * SHARE))))
            .child(self.fields(cx))
            .child(self.list(cx))
            .when(self.results.is_some(), |el| el.child(self.foot()));
        let panel = crate::kit::slide_fade(panel, "search-open", theme.spacing.xs, Pace::Fade, cx);
        let root = crate::kit::anchor(&theme, window)
            .id("search-backdrop")
            .child(panel)
            .capture_action(cx.listener(|this, _: &MoveUp, _window, cx| this.step(-1, cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, _window, cx| this.step(1, cx)))
            .capture_action(cx.listener(|_this, _: &Escape, _window, cx| {
                cx.emit(ProjectSearchEvent::Dismiss);
            }))
            .on_action(cx.listener(|this, _: &ToggleMatchCase, _window, cx| {
                this.toggle(|t| &mut t.match_case, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleWholeWord, _window, cx| {
                this.toggle(|t| &mut t.whole_word, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, _window, cx| {
                this.toggle(|t| &mut t.regex, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _window, cx| {
                    cx.emit(ProjectSearchEvent::Dismiss);
                    cx.stop_propagation();
                }),
            );
        gpui::deferred(root).with_priority(Layer::Dialog.priority())
    }
}

/// A line's text as drawn, with an ellipsis where it was cut, and where its matches are in
/// that text.
fn shown_line(hit: &LineHit) -> (SharedString, Vec<std::ops::Range<usize>>) {
    const CUT: &str = "\u{2026}";
    let lead = if hit.cut_before { CUT.len() } else { 0 };
    let mut text =
        String::with_capacity(hit.text.len().saturating_add(CUT.len().saturating_mul(2)));
    if hit.cut_before {
        text.push_str(CUT);
    }
    text.push_str(&hit.text);
    if hit.cut_after {
        text.push_str(CUT);
    }
    let marks = hit
        .spans
        .iter()
        .filter_map(|span| {
            let start = usize::try_from(span.start).ok()?.saturating_add(lead);
            let end = usize::try_from(span.end).ok()?.saturating_add(lead);
            (start < end && text.is_char_boundary(start) && text.is_char_boundary(end))
                .then_some(start..end)
        })
        .collect();
    (text.into(), marks)
}

/// `n` with a thin space between each three figures: "2 000".
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (ix, c) in digits.chars().enumerate() {
        if ix > 0 && (digits.len().saturating_sub(ix)) % 3 == 0 {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

fn result_label(n: u32) -> String {
    match n {
        1 => "1 result".to_owned(),
        n => format!("{} results", thousands(n)),
    }
}

fn file_label(n: usize) -> String {
    match n {
        1 => "1 file".to_owned(),
        n => format!("{} files", thousands(u32::try_from(n).unwrap_or(u32::MAX))),
    }
}

fn count_label(n: usize) -> String {
    match n {
        1 => "1 match".to_owned(),
        n => format!("{n} matches"),
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::search::Span;

    use super::*;

    /// A cut line reads with an ellipsis at each cut end, and its matches move with the text.
    #[test]
    fn a_cut_line_shows_its_cuts_and_keeps_its_matches() {
        let hit = LineHit {
            line: 3,
            text: "let needle = 1;".to_owned(),
            spans: vec![Span { start: 4, end: 10 }],
            cut_before: true,
            cut_after: true,
        };
        let (text, marks) = shown_line(&hit);
        assert_eq!(text.as_ref(), "\u{2026}let needle = 1;\u{2026}");
        let mark = marks.first().cloned().unwrap();
        assert_eq!(text.get(mark), Some("needle"));
    }

    #[test]
    fn counts_read_as_a_person_says_them() {
        assert_eq!(thousands(2_000), "2\u{2009}000");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1\u{2009}234\u{2009}567");
        assert_eq!(result_label(1), "1 result");
        assert_eq!(file_label(3), "3 files");
    }
}
